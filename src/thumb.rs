use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::ImageOutputFormat;

/// Returns the cache path for a thumbnail and generates it if needed.
/// Returns the thumbnail bytes on success.
pub async fn get_or_create(
    source: &Path,
    cache_dir: &Path,
    dir_idx: usize,
    relative_path: &str,
    size: u32,
    semaphore: &tokio::sync::Semaphore,
) -> Result<Vec<u8>, String> {
    let cache_path = thumb_cache_path(cache_dir, dir_idx, relative_path);

    // Check if cached thumbnail is still valid
    if cache_path.exists() {
        if let (Ok(src_meta), Ok(cache_meta)) =
            (std::fs::metadata(source), std::fs::metadata(&cache_path))
        {
            if let (Ok(src_time), Ok(cache_time)) = (src_meta.modified(), cache_meta.modified()) {
                if cache_time > src_time {
                    return tokio::fs::read(&cache_path)
                        .await
                        .map_err(|e| e.to_string());
                }
            }
        }
    }

    // Generate thumbnail (limit concurrency to control memory)
    let _permit = semaphore.acquire().await.map_err(|e| e.to_string())?;

    let source = source.to_owned();
    let cache_path_clone = cache_path.clone();

    let bytes = tokio::task::spawn_blocking(move || generate(&source, &cache_path_clone, size))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

    Ok(bytes)
}

fn generate(source: &Path, cache_path: &Path, size: u32) -> Result<Vec<u8>, String> {
    let img = image::open(source).map_err(|e| format!("failed to open image: {}", e))?;

    let thumb = img.resize(size, size, FilterType::Triangle);

    let mut buf = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut buf);
    thumb
        .write_to(&mut cursor, ImageOutputFormat::Jpeg(70))
        .map_err(|e| format!("failed to encode thumbnail: {}", e))?;

    // Write to cache
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(cache_path, &buf).ok();

    Ok(buf)
}

fn thumb_cache_path(cache_dir: &Path, dir_idx: usize, relative_path: &str) -> PathBuf {
    cache_dir
        .join("thumbs")
        .join(dir_idx.to_string())
        .join(relative_path)
}
