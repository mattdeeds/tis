# tis - Thumbnail Image Server

A tiny web server that serves images from local directories with a mobile-friendly browser UI. Designed for low-power single board computers.

## Build

```
cargo build --release
```

The binary will be at `target/release/tis` (~1.9MB).

## Configure

Copy `config.example.toml` to `config.toml` and edit it:

```toml
[server]
bind = "0.0.0.0:8080"
cache_dir = "/home/pi/.cache/tis"
state_file = "/home/pi/.local/share/tis/state.json"
thumb_size = 300

# Optional HTTPS:
# tls_cert = "/path/to/cert.pem"
# tls_key = "/path/to/key.pem"

[[directories]]
name = "Photos"
path = "/home/pi/photos"

[[directories]]
name = "Camera"
path = "/mnt/sdcard/DCIM"
```

- `bind` - Address and port to listen on
- `cache_dir` - Where generated thumbnails are cached
- `state_file` - Where download marks are persisted
- `thumb_size` - Thumbnail width/height in pixels
- `tls_cert`/`tls_key` - PEM files for HTTPS (optional)
- `[[directories]]` - Add as many as you need, each with a `name` and `path`

## Run

```
tis config.toml
```

Then open `http://<your-ip>:8080` on your phone.

## Features

- Browse directories and view images in a responsive grid
- Thumbnails generated on first access, cached to disk
- Tap a thumbnail to open the full-resolution image
- Download button for saving images
- Mark images as downloaded (persisted across restarts)
- Optional HTTPS via rustls
