use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tokio_util::io::ReaderStream;

use crate::AppState;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/browse/{dir_idx}", get(browse_root))
        .route("/browse/{dir_idx}/{*path}", get(browse_sub))
        .route("/thumb/{dir_idx}/{*path}", get(serve_thumb))
        .route("/image/{dir_idx}/{*path}", get(serve_image))
        .route("/download/{dir_idx}/{*path}", get(serve_download))
        .route("/mark/{dir_idx}/{*path}", post(toggle_mark))
        .with_state(state)
}

// --- Handlers ---

async fn index(State(state): State<Arc<AppState>>) -> Html<String> {
    let mut dirs_html = String::with_capacity(state.config.directories.len() * 128);
    for (i, dir) in state.config.directories.iter().enumerate() {
        let exists = state.canonical_dirs[i].is_some();
        let class = if exists { "dir-link" } else { "dir-link disabled" };
        write_html!(
            dirs_html,
            r#"<a class="{}" href="/browse/{}">{} {}</a>"#,
            class,
            i,
            FOLDER_ICON,
            HtmlEsc(&dir.name)
        );
    }

    Html(page(
        "tis - directories",
        &format!(r#"<h1>Directories</h1><div class="dir-list">{dirs_html}</div>"#),
    ))
}

async fn browse_root(
    State(state): State<Arc<AppState>>,
    AxumPath(dir_idx): AxumPath<usize>,
) -> Response {
    browse_inner(&state, dir_idx, "").await
}

async fn browse_sub(
    State(state): State<Arc<AppState>>,
    AxumPath((dir_idx, path)): AxumPath<(usize, String)>,
) -> Response {
    browse_inner(&state, dir_idx, &path).await
}

async fn browse_inner(state: &AppState, dir_idx: usize, sub_path: &str) -> Response {
    let Some(dir_config) = state.config.directories.get(dir_idx) else {
        return error_page(StatusCode::NOT_FOUND, "Directory not found");
    };

    let Some(base) = state.canonical_dirs.get(dir_idx).and_then(|d| d.as_ref()) else {
        return error_page(StatusCode::NOT_FOUND, "Directory not accessible");
    };

    let full_path = if sub_path.is_empty() {
        base.clone()
    } else {
        match base.join(sub_path).canonicalize() {
            Ok(p) => p,
            Err(_) => return error_page(StatusCode::NOT_FOUND, "Path not found"),
        }
    };

    if !full_path.starts_with(base) {
        return error_page(StatusCode::FORBIDDEN, "Access denied");
    }

    if !full_path.is_dir() {
        return error_page(StatusCode::NOT_FOUND, "Not a directory");
    }

    let entries = match std::fs::read_dir(&full_path) {
        Ok(e) => e,
        Err(_) => return error_page(StatusCode::INTERNAL_SERVER_ERROR, "Cannot read directory"),
    };

    let mut subdirs: Vec<String> = Vec::new();
    let mut images: Vec<String> = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            subdirs.push(name);
        } else if ft.is_file() && is_image(&name) {
            images.push(name);
        }
    }

    subdirs.sort_unstable_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
    images.sort_unstable_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));

    let downloads = state.downloads.read().await;

    // Build breadcrumb
    let mut breadcrumb = String::with_capacity(256);
    write_html!(
        breadcrumb,
        r#"<nav class="breadcrumb"><a href="/">Home</a> / <a href="/browse/{}">{}</a>"#,
        dir_idx,
        HtmlEsc(&dir_config.name)
    );
    if !sub_path.is_empty() {
        let parts: Vec<&str> = sub_path.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            let partial = parts[..=i].join("/");
            write_html!(
                breadcrumb,
                r#" / <a href="/browse/{}/{}">{}</a>"#,
                dir_idx,
                UrlEnc(&partial),
                HtmlEsc(part)
            );
        }
    }
    breadcrumb.push_str("</nav>");

    // Build subdirectory list
    let mut dirs_html = String::with_capacity(subdirs.len() * 128);
    for name in &subdirs {
        let link_path = if sub_path.is_empty() {
            url_encode(name)
        } else {
            format!("{}/{}", url_encode(sub_path), url_encode(name))
        };
        write_html!(
            dirs_html,
            r#"<a class="dir-link" href="/browse/{}/{}">{} {}</a>"#,
            dir_idx,
            link_path,
            FOLDER_ICON,
            HtmlEsc(name)
        );
    }

    // Build image grid
    let mut grid_html = String::with_capacity(images.len() * 512);
    for name in &images {
        let rel_path = if sub_path.is_empty() {
            url_encode(name)
        } else {
            format!("{}/{}", url_encode(sub_path), url_encode(name))
        };

        let abs_path = full_path.join(name).to_string_lossy().into_owned();
        let is_marked = downloads.marked.contains(&abs_path);
        let card_class = if is_marked { "card downloaded" } else { "card" };
        let mark_active = if is_marked { " active" } else { "" };

        use std::fmt::Write;
        write!(
            grid_html,
            r#"<div class="{card_class}" id="c{hash}">
<a href="/image/{dir_idx}/{rel_path}" target="_blank" rel="noopener">
<img src="/thumb/{dir_idx}/{rel_path}" alt="{name}" loading="lazy">
</a><div class="card-info">
<span class="card-name" title="{name}">{name}</span>
<div class="card-actions">
<a class="btn btn-dl" href="/download/{dir_idx}/{rel_path}" title="Download">&#8615;</a>
<button class="btn btn-mark{mark_active}" onclick="toggleMark({dir_idx},'{rel_js}',this)" title="Mark downloaded">&#10003;</button>
</div></div></div>"#,
            hash = simple_hash(&abs_path),
            name = HtmlEsc(name),
            rel_path = rel_path,
            rel_js = JsEsc(&rel_path),
        )
        .unwrap();
    }

    drop(downloads);

    let count_info = format!(
        "{} folder{}, {} image{}",
        subdirs.len(),
        if subdirs.len() == 1 { "" } else { "s" },
        images.len(),
        if images.len() == 1 { "" } else { "s" },
    );

    let dirs_section = if dirs_html.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="dir-list">{dirs_html}</div>"#)
    };
    let grid_section = if grid_html.is_empty() {
        r#"<p class="empty">No images in this directory.</p>"#.to_string()
    } else {
        format!(r#"<div class="grid">{grid_html}</div>"#)
    };

    let body = format!(
        r#"{breadcrumb}
<p class="count-info">{count_info}</p>
{dirs_section}
{grid_section}
<script>{JS}</script>"#,
    );

    Html(page("tis - browse", &body)).into_response()
}

async fn serve_thumb(
    State(state): State<Arc<AppState>>,
    AxumPath((dir_idx, path)): AxumPath<(usize, String)>,
) -> Response {
    let Some((source_path, relative)) = resolve_image_path(&state, dir_idx, &path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    match crate::thumb::get_or_create(
        &source_path,
        &state.config.server.cache_dir,
        dir_idx,
        &relative,
        state.config.server.thumb_size,
        &state.thumb_semaphore,
    )
    .await
    {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, "image/jpeg"),
             (header::CACHE_CONTROL, "public, max-age=86400")],
            bytes,
        )
            .into_response(),
        Err(e) => {
            eprintln!("thumbnail error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Stream full-resolution images from disk to avoid loading entirely into memory.
async fn serve_image(
    State(state): State<Arc<AppState>>,
    AxumPath((dir_idx, path)): AxumPath<(usize, String)>,
) -> Response {
    let Some((source_path, _)) = resolve_image_path(&state, dir_idx, &path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    stream_file(&source_path, false).await
}

async fn serve_download(
    State(state): State<Arc<AppState>>,
    AxumPath((dir_idx, path)): AxumPath<(usize, String)>,
) -> Response {
    let Some((source_path, _)) = resolve_image_path(&state, dir_idx, &path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    stream_file(&source_path, true).await
}

/// Stream a file from disk with proper headers.
async fn stream_file(path: &Path, download: bool) -> Response {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let metadata = match file.metadata().await {
        Ok(m) => m,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let mime = mime_for_path(path);
    let stream = ReaderStream::with_capacity(file, 64 * 1024);
    let body = Body::from_stream(stream);

    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, metadata.len())
        .header(header::CACHE_CONTROL, "public, max-age=3600");

    if download {
        let filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        builder = builder.header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        );
    }

    builder.body(body).unwrap().into_response()
}

async fn toggle_mark(
    State(state): State<Arc<AppState>>,
    AxumPath((dir_idx, path)): AxumPath<(usize, String)>,
) -> Response {
    let Some((source_path, _)) = resolve_image_path(&state, dir_idx, &path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let abs = source_path.to_string_lossy().into_owned();
    let marked;

    {
        let mut downloads = state.downloads.write().await;
        if downloads.marked.contains(&abs) {
            downloads.marked.remove(&abs);
            marked = false;
        } else {
            downloads.marked.insert(abs);
            marked = true;
        }
    }

    // Signal debounced saver instead of spawning per-mark
    state.save_notify.notify_one();

    let body = if marked {
        r#"{"marked":true}"#
    } else {
        r#"{"marked":false}"#
    };
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

// --- Helpers ---

fn resolve_image_path(
    state: &AppState,
    dir_idx: usize,
    sub_path: &str,
) -> Option<(PathBuf, String)> {
    let base = state.canonical_dirs.get(dir_idx)?.as_ref()?;
    let full = base.join(sub_path).canonicalize().ok()?;

    if !full.starts_with(base) || !full.is_file() {
        return None;
    }

    let relative = full.strip_prefix(base).ok()?.to_string_lossy().into_owned();
    Some((full, relative))
}

fn is_image(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.rsplit('.').next(),
        Some("jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tiff" | "tif")
    )
}

fn mime_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tiff" | "tif" => "image/tiff",
        _ => "application/octet-stream",
    }
}

// --- Efficient string formatters ---

const HEX: &[u8; 16] = b"0123456789ABCDEF";
const FOLDER_ICON: &str = "<span class=\"folder-icon\">&#128193;</span>";

/// Display adapter for HTML escaping (single-pass, zero intermediate alloc).
struct HtmlEsc<'a>(&'a str);
impl std::fmt::Display for HtmlEsc<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for ch in self.0.chars() {
            match ch {
                '&' => f.write_str("&amp;")?,
                '<' => f.write_str("&lt;")?,
                '>' => f.write_str("&gt;")?,
                '"' => f.write_str("&quot;")?,
                c => f.write_str(c.encode_utf8(&mut [0; 4]))?,
            }
        }
        Ok(())
    }
}

/// Display adapter for URL encoding (preserves /).
struct UrlEnc<'a>(&'a str);
impl std::fmt::Display for UrlEnc<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in self.0.bytes() {
            match b {
                b'/' => f.write_str("/")?,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    let buf = [b];
                    f.write_str(unsafe { std::str::from_utf8_unchecked(&buf) })?
                }
                _ => {
                    let buf = [HEX[(b >> 4) as usize], HEX[(b & 0xf) as usize]];
                    f.write_str("%")?;
                    f.write_str(unsafe { std::str::from_utf8_unchecked(&buf) })?;
                }
            }
        }
        Ok(())
    }
}

/// Display adapter for JS string escaping.
struct JsEsc<'a>(&'a str);
impl std::fmt::Display for JsEsc<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for ch in self.0.chars() {
            match ch {
                '\\' => f.write_str("\\\\")?,
                '\'' => f.write_str("\\'")?,
                '\n' => f.write_str("\\n")?,
                c => write!(f, "{c}")?,
            }
        }
        Ok(())
    }
}

/// Standalone url_encode function for cases where we need a String.
fn url_encode(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    write!(out, "{}", UrlEnc(s)).unwrap();
    out
}

/// Simple non-cryptographic hash for DOM IDs.
fn simple_hash(s: &str) -> u64 {
    let mut hash: u64 = 5381;
    for b in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    hash
}

/// Macro for formatted HTML writes into a String.
macro_rules! write_html {
    ($dst:expr, $($arg:tt)*) => {
        {
            use std::fmt::Write;
            write!($dst, $($arg)*).unwrap();
        }
    };
}
use write_html;

fn error_page(status: StatusCode, message: &str) -> Response {
    (
        status,
        Html(page(
            "tis - error",
            &format!(
                r#"<h1>Error {}</h1><p>{}</p><a href="/">Back to home</a>"#,
                status.as_u16(),
                HtmlEsc(message)
            ),
        )),
    )
        .into_response()
}

const JS: &str = r#"
async function toggleMark(dirIdx, path, btn) {
    try {
        const res = await fetch('/mark/' + dirIdx + '/' + path, {method: 'POST'});
        if (res.ok) {
            const data = await res.json();
            const card = btn.closest('.card');
            if (data.marked) {
                card.classList.add('downloaded');
                btn.classList.add('active');
            } else {
                card.classList.remove('downloaded');
                btn.classList.remove('active');
            }
        }
    } catch (e) {
        console.error('mark failed:', e);
    }
}
"#;

const CSS: &str = r#"
*,*::before,*::after{box-sizing:border-box}
body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;margin:0;padding:0;background:#f5f5f5;color:#333}
h1{font-size:1.4em;margin:12px 16px}
.breadcrumb{padding:10px 16px;background:#fff;border-bottom:1px solid #e0e0e0;font-size:.9em;overflow-x:auto;white-space:nowrap}
.breadcrumb a{color:#1976d2;text-decoration:none}
.breadcrumb a:hover{text-decoration:underline}
.count-info{margin:8px 16px;font-size:.85em;color:#777}
.dir-list{display:flex;flex-direction:column;gap:2px;margin:8px 16px}
.dir-link{display:flex;align-items:center;gap:8px;padding:10px 12px;background:#fff;border-radius:6px;text-decoration:none;color:#333;font-size:.95em}
.dir-link:hover{background:#e3f2fd}
.dir-link.disabled{opacity:.5;pointer-events:none}
.folder-icon{font-size:1.2em}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(150px,1fr));gap:8px;padding:8px 16px 24px}
.card{background:#fff;border-radius:8px;overflow:hidden;box-shadow:0 1px 3px rgba(0,0,0,.08);border:2px solid transparent;transition:border-color .2s}
.card.downloaded{border-color:#4caf50}
.card img{width:100%;aspect-ratio:1;object-fit:cover;display:block;background:#eee}
.card-info{padding:4px 8px 6px}
.card-name{display:block;font-size:.75em;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;color:#555}
.card-actions{display:flex;gap:4px;margin-top:4px}
.btn{display:inline-flex;align-items:center;justify-content:center;width:32px;height:28px;border:1px solid #ddd;border-radius:4px;background:#fafafa;color:#555;text-decoration:none;font-size:1.1em;cursor:pointer;padding:0}
.btn:hover{background:#e0e0e0}
.btn-mark.active{background:#4caf50;color:#fff;border-color:#4caf50}
.empty{margin:24px 16px;color:#999}
"#;

fn page(title: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<style>{CSS}</style>
</head>
<body>
{body}
</body>
</html>"#,
    )
}
