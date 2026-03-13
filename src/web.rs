use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

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
    let mut dirs_html = String::new();
    for (i, dir) in state.config.directories.iter().enumerate() {
        let exists = dir.path.is_dir();
        let class = if exists { "dir-link" } else { "dir-link disabled" };
        dirs_html.push_str(&format!(
            r#"<a class="{class}" href="/browse/{i}"><span class="folder-icon">&#128193;</span> {name}</a>"#,
            name = html_escape(&dir.name),
        ));
    }

    Html(page(
        "tis - directories",
        None,
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

    let base = match dir_config.path.canonicalize() {
        Ok(p) => p,
        Err(_) => return error_page(StatusCode::NOT_FOUND, "Directory not accessible"),
    };

    let full_path = if sub_path.is_empty() {
        base.clone()
    } else {
        base.join(sub_path)
    };

    let full_path = match full_path.canonicalize() {
        Ok(p) => p,
        Err(_) => return error_page(StatusCode::NOT_FOUND, "Path not found"),
    };

    // Security: ensure path is within the configured directory
    if !full_path.starts_with(&base) {
        return error_page(StatusCode::FORBIDDEN, "Access denied");
    }

    if !full_path.is_dir() {
        return error_page(StatusCode::NOT_FOUND, "Not a directory");
    }

    let mut entries = match std::fs::read_dir(&full_path) {
        Ok(e) => e,
        Err(_) => return error_page(StatusCode::INTERNAL_SERVER_ERROR, "Cannot read directory"),
    };

    let mut subdirs: Vec<String> = Vec::new();
    let mut images: Vec<String> = Vec::new();

    while let Some(Ok(entry)) = entries.next() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
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
    let mut breadcrumb = format!(
        r#"<nav class="breadcrumb"><a href="/">Home</a> / <a href="/browse/{dir_idx}">{dir_name}</a>"#,
        dir_name = html_escape(&dir_config.name),
    );
    if !sub_path.is_empty() {
        let parts: Vec<&str> = sub_path.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            let partial = parts[..=i].join("/");
            breadcrumb.push_str(&format!(
                r#" / <a href="/browse/{dir_idx}/{path}">{name}</a>"#,
                path = url_encode(&partial),
                name = html_escape(part),
            ));
        }
    }
    breadcrumb.push_str("</nav>");

    // Build subdirectory list
    let mut dirs_html = String::new();
    for name in &subdirs {
        let link_path = if sub_path.is_empty() {
            url_encode(name)
        } else {
            format!("{}/{}", url_encode(sub_path), url_encode(name))
        };
        dirs_html.push_str(&format!(
            r#"<a class="dir-link" href="/browse/{dir_idx}/{path}"><span class="folder-icon">&#128193;</span> {name}</a>"#,
            path = link_path,
            name = html_escape(name),
        ));
    }

    // Build image grid
    let mut grid_html = String::new();
    for name in &images {
        let rel_path = if sub_path.is_empty() {
            url_encode(name)
        } else {
            format!("{}/{}", url_encode(sub_path), url_encode(name))
        };

        let abs_path = full_path.join(name).to_string_lossy().into_owned();
        let is_marked = downloads.marked.contains(&abs_path);
        let card_class = if is_marked { "card downloaded" } else { "card" };

        grid_html.push_str(&format!(
            r#"<div class="{card_class}" id="card-{hash}">
  <a href="/image/{dir_idx}/{rel_path}" target="_blank" rel="noopener">
    <img src="/thumb/{dir_idx}/{rel_path}" alt="{name}" loading="lazy">
  </a>
  <div class="card-info">
    <span class="card-name" title="{name}">{name}</span>
    <div class="card-actions">
      <a class="btn btn-dl" href="/download/{dir_idx}/{rel_path}" title="Download">&#8615;</a>
      <button class="btn btn-mark{mark_active}" onclick="toggleMark({dir_idx},'{rel_path_js}',this)" title="Mark downloaded">&#10003;</button>
    </div>
  </div>
</div>"#,
            hash = simple_hash(&abs_path),
            name = html_escape(name),
            rel_path = rel_path,
            rel_path_js = js_escape(&rel_path),
            mark_active = if is_marked { " active" } else { "" },
        ));
    }

    drop(downloads);

    let count_info = format!(
        "{} folder{}, {} image{}",
        subdirs.len(),
        if subdirs.len() == 1 { "" } else { "s" },
        images.len(),
        if images.len() == 1 { "" } else { "s" },
    );

    let body = format!(
        r#"{breadcrumb}
<p class="count-info">{count_info}</p>
{dirs_section}
{grid_section}
<script>{JS}</script>"#,
        dirs_section = if dirs_html.is_empty() {
            String::new()
        } else {
            format!(r#"<div class="dir-list">{dirs_html}</div>"#)
        },
        grid_section = if grid_html.is_empty() {
            "<p class=\"empty\">No images in this directory.</p>".to_string()
        } else {
            format!(r#"<div class="grid">{grid_html}</div>"#)
        },
    );

    Html(page("tis - browse", None, &body)).into_response()
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
            [(header::CONTENT_TYPE, "image/jpeg")],
            [(header::CACHE_CONTROL, "public, max-age=86400")],
            bytes,
        )
            .into_response(),
        Err(e) => {
            eprintln!("thumbnail error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn serve_image(
    State(state): State<Arc<AppState>>,
    AxumPath((dir_idx, path)): AxumPath<(usize, String)>,
) -> Response {
    let Some((source_path, _)) = resolve_image_path(&state, dir_idx, &path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mime = mime_for_path(&source_path);

    match tokio::fs::read(&source_path).await {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, mime)],
            [(header::CACHE_CONTROL, "public, max-age=3600")],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve_download(
    State(state): State<Arc<AppState>>,
    AxumPath((dir_idx, path)): AxumPath<(usize, String)>,
) -> Response {
    let Some((source_path, _)) = resolve_image_path(&state, dir_idx, &path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let filename = source_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let mime = mime_for_path(&source_path);

    match tokio::fs::read(&source_path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, mime),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", filename),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
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

    // Save state in background
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = state.save_state().await {
            eprintln!("failed to save state: {}", e);
        }
    });

    axum::Json(serde_json::json!({ "marked": marked })).into_response()
}

// --- Helpers ---

fn resolve_image_path(state: &AppState, dir_idx: usize, sub_path: &str) -> Option<(PathBuf, String)> {
    let dir_config = state.config.directories.get(dir_idx)?;
    let base = dir_config.path.canonicalize().ok()?;
    let full = base.join(sub_path).canonicalize().ok()?;

    if !full.starts_with(&base) || !full.is_file() {
        return None;
    }

    let relative = full.strip_prefix(&base).ok()?.to_string_lossy().into_owned();
    Some((full, relative))
}

fn is_image(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.rsplit('.').next(),
        Some("jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tiff" | "tif")
    )
}

fn mime_for_path(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tiff" | "tif" => "image/tiff",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn url_encode(s: &str) -> String {
    s.split('/')
        .map(|segment| {
            segment
                .bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        String::from(b as char)
                    }
                    _ => format!("%{:02X}", b),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn js_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
}

fn simple_hash(s: &str) -> u64 {
    let mut hash: u64 = 5381;
    for b in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    hash
}

fn error_page(status: StatusCode, message: &str) -> Response {
    (
        status,
        Html(page(
            "tis - error",
            None,
            &format!(
                r#"<h1>Error {}</h1><p>{}</p><a href="/">Back to home</a>"#,
                status.as_u16(),
                html_escape(message)
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
*, *::before, *::after { box-sizing: border-box; }
body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    margin: 0; padding: 0;
    background: #f5f5f5; color: #333;
}
h1 { font-size: 1.4em; margin: 12px 16px; }
.breadcrumb {
    padding: 10px 16px;
    background: #fff;
    border-bottom: 1px solid #e0e0e0;
    font-size: 0.9em;
    overflow-x: auto;
    white-space: nowrap;
}
.breadcrumb a { color: #1976d2; text-decoration: none; }
.breadcrumb a:hover { text-decoration: underline; }
.count-info { margin: 8px 16px; font-size: 0.85em; color: #777; }
.dir-list {
    display: flex; flex-direction: column;
    gap: 2px; margin: 8px 16px;
}
.dir-link {
    display: flex; align-items: center; gap: 8px;
    padding: 10px 12px;
    background: #fff;
    border-radius: 6px;
    text-decoration: none; color: #333;
    font-size: 0.95em;
}
.dir-link:hover { background: #e3f2fd; }
.dir-link.disabled { opacity: 0.5; pointer-events: none; }
.folder-icon { font-size: 1.2em; }
.grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    gap: 8px;
    padding: 8px 16px 24px;
}
.card {
    background: #fff;
    border-radius: 8px;
    overflow: hidden;
    box-shadow: 0 1px 3px rgba(0,0,0,0.08);
    border: 2px solid transparent;
    transition: border-color 0.2s;
}
.card.downloaded { border-color: #4caf50; }
.card img {
    width: 100%;
    aspect-ratio: 1;
    object-fit: cover;
    display: block;
    background: #eee;
}
.card-info {
    padding: 4px 8px 6px;
}
.card-name {
    display: block;
    font-size: 0.75em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: #555;
}
.card-actions {
    display: flex;
    gap: 4px;
    margin-top: 4px;
}
.btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 32px; height: 28px;
    border: 1px solid #ddd;
    border-radius: 4px;
    background: #fafafa;
    color: #555;
    text-decoration: none;
    font-size: 1.1em;
    cursor: pointer;
    padding: 0;
}
.btn:hover { background: #e0e0e0; }
.btn-mark.active { background: #4caf50; color: #fff; border-color: #4caf50; }
.empty { margin: 24px 16px; color: #999; }
"#;

fn page(title: &str, extra_head: Option<&str>, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>{CSS}</style>
{extra}
</head>
<body>
{body}
</body>
</html>"#,
        extra = extra_head.unwrap_or(""),
    )
}
