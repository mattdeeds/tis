use std::path::Path;

/// Get or create a cached thumbnail. Returns JPEG bytes.
pub async fn get_or_create(
    source: &Path,
    cache_dir: &Path,
    dir_idx: usize,
    relative_path: &str,
    size: u32,
    semaphore: &tokio::sync::Semaphore,
) -> Result<Vec<u8>, String> {
    let cache_path = cache_dir
        .join("thumbs")
        .join(dir_idx.to_string())
        .join(relative_path);

    // Check if cached thumbnail is still valid
    if let Ok(cache_meta) = std::fs::metadata(&cache_path) {
        if let Ok(src_meta) = std::fs::metadata(source) {
            if let (Ok(ct), Ok(st)) = (cache_meta.modified(), src_meta.modified()) {
                if ct > st {
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
    let size = size as u16;

    tokio::task::spawn_blocking(move || {
        let bytes = generate_jpeg(&source, size)?;

        // Cache to disk
        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&cache_path, &bytes).ok();

        Ok(bytes)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Decode JPEG, resize with progressive downsampling, re-encode as JPEG.
fn generate_jpeg(source: &Path, target_size: u16) -> Result<Vec<u8>, String> {
    let data = std::fs::read(source).map_err(|e| format!("read: {}", e))?;

    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;

    let mut decoder = zune_jpeg::JpegDecoder::new(ZCursor::new(&data));
    let pixels = decoder
        .decode()
        .map_err(|e| format!("decode: {}", e))?;

    let (sw, sh) = decoder
        .dimensions()
        .ok_or("no image dimensions")?;

    let colorspace = decoder.output_colorspace().unwrap_or(ColorSpace::RGB);

    // Free compressed data and decoder state before resize
    drop(decoder);
    drop(data);

    let rgb: Vec<u8> = match colorspace {
        ColorSpace::RGB => pixels,
        ColorSpace::Luma => {
            let mut rgb = Vec::with_capacity(sw * sh * 3);
            for &g in &pixels {
                rgb.push(g);
                rgb.push(g);
                rgb.push(g);
            }
            rgb
        }
        ColorSpace::YCbCr => {
            // zune-jpeg should auto-convert YCbCr to RGB, but handle just in case
            pixels
        }
        _ => return Err(format!("unsupported colorspace: {:?}", colorspace)),
    };

    let target = target_size as usize;
    let (dw, dh) = fit_dimensions(sw, sh, target);

    // Don't upscale
    if dw >= sw && dh >= sh {
        return encode_jpeg(&rgb, sw as u16, sh as u16);
    }

    // Progressive downsample: fast 2x box filter until close to target,
    // then final area-average resize for quality.
    let (mut buf, mut w, mut h): (Vec<u8>, usize, usize) = (rgb, sw, sh);

    while w / 2 >= dw * 2 && h / 2 >= dh * 2 && w >= 4 && h >= 4 {
        let (new_buf, nw, nh) = downsample_2x(&buf, w, h);
        buf = new_buf;
        w = nw;
        h = nh;
    }

    if w != dw || h != dh {
        buf = resize_area_avg(&buf, w, h, dw, dh);
    }

    encode_jpeg(&buf, dw as u16, dh as u16)
}

fn encode_jpeg(rgb: &[u8], w: u16, h: u16) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(w as usize * h as usize / 4);
    let encoder = jpeg_encoder::Encoder::new(&mut buf, 70);
    encoder
        .encode(rgb, w, h, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| format!("encode: {}", e))?;
    Ok(buf)
}

fn fit_dimensions(sw: usize, sh: usize, target: usize) -> (usize, usize) {
    if sw == 0 || sh == 0 {
        return (1, 1);
    }
    if sw >= sh {
        let dw = target;
        let dh = (sh * target + sw / 2) / sw;
        (dw, dh.max(1))
    } else {
        let dh = target;
        let dw = (sw * target + sh / 2) / sh;
        (dw.max(1), dh)
    }
}

/// Fast 2x box downsample for RGB images.
fn downsample_2x(src: &[u8], sw: usize, sh: usize) -> (Vec<u8>, usize, usize) {
    let dw = sw / 2;
    let dh = sh / 2;
    let src_stride = sw * 3;
    let dst_stride = dw * 3;
    let mut dst = vec![0u8; dw * dh * 3];

    for dy in 0..dh {
        let sy = dy * 2;
        let src_row0 = sy * src_stride;
        let src_row1 = src_row0 + src_stride;
        let dst_row = dy * dst_stride;

        for dx in 0..dw {
            let sx3 = dx * 2 * 3;
            let di = dst_row + dx * 3;

            for c in 0..3 {
                let sum = src[src_row0 + sx3 + c] as u32
                    + src[src_row0 + sx3 + 3 + c] as u32
                    + src[src_row1 + sx3 + c] as u32
                    + src[src_row1 + sx3 + 3 + c] as u32;
                dst[di + c] = ((sum + 2) / 4) as u8;
            }
        }
    }

    (dst, dw, dh)
}

/// Area-averaging downscale for RGB images. Final quality pass.
fn resize_area_avg(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut dst = vec![0u8; dw * dh * 3];
    let src_stride = sw * 3;

    for dy in 0..dh {
        let sy_start = dy * sh / dh;
        let sy_end = ((dy + 1) * sh / dh).min(sh);

        for dx in 0..dw {
            let sx_start = dx * sw / dw;
            let sx_end = ((dx + 1) * sw / dw).min(sw);

            let mut r: u32 = 0;
            let mut g: u32 = 0;
            let mut b: u32 = 0;
            let mut count: u32 = 0;

            for sy in sy_start..sy_end {
                let row = sy * src_stride;
                for sx in sx_start..sx_end {
                    let idx = row + sx * 3;
                    r += src[idx] as u32;
                    g += src[idx + 1] as u32;
                    b += src[idx + 2] as u32;
                    count += 1;
                }
            }

            let didx = (dy * dw + dx) * 3;
            if count > 0 {
                dst[didx] = (r / count) as u8;
                dst[didx + 1] = (g / count) as u8;
                dst[didx + 2] = (b / count) as u8;
            }
        }
    }

    dst
}
