use std::collections::HashSet;
use std::io::BufReader;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, Semaphore};

mod thumb;
mod web;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub directories: Vec<DirConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub cache_dir: PathBuf,
    pub state_file: PathBuf,
    #[serde(default = "default_thumb_size")]
    pub thumb_size: u32,
}

fn default_thumb_size() -> u32 {
    300
}

#[derive(Debug, Deserialize)]
pub struct DirConfig {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct DownloadState {
    pub marked: HashSet<String>,
}

pub struct AppState {
    pub config: Config,
    pub downloads: RwLock<DownloadState>,
    pub thumb_semaphore: Semaphore,
}

impl AppState {
    pub async fn save_state(&self) -> std::io::Result<()> {
        let state = self.downloads.read().await;
        let json = serde_json::to_string(&*state)?;
        if let Some(parent) = self.config.server.state_file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.config.server.state_file, &json).await
    }
}

fn load_tls_config(
    cert_path: &PathBuf,
    key_path: &PathBuf,
) -> Result<rustls::ServerConfig, Box<dyn std::error::Error>> {
    let cert_file = std::fs::File::open(cert_path)?;
    let key_file = std::fs::File::open(key_path)?;

    let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .collect::<Result<_, _>>()?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(key_file))?
        .ok_or("no private key found in key file")?;

    let config = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(certs, key)?;

    Ok(config)
}

#[tokio::main]
async fn main() {
    let config_path = std::env::args().nth(1).unwrap_or_else(|| "config.toml".into());

    let config_str = std::fs::read_to_string(&config_path).unwrap_or_else(|e| {
        eprintln!("failed to read config file '{}': {}", config_path, e);
        std::process::exit(1);
    });

    let config: Config = toml::from_str(&config_str).unwrap_or_else(|e| {
        eprintln!("failed to parse config: {}", e);
        std::process::exit(1);
    });

    // Validate directories exist
    for dir in &config.directories {
        if !dir.path.is_dir() {
            eprintln!("warning: directory '{}' does not exist: {}", dir.name, dir.path.display());
        }
    }

    // Load download state
    let downloads = if config.server.state_file.exists() {
        match std::fs::read_to_string(&config.server.state_file) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
            Err(_) => DownloadState::default(),
        }
    } else {
        DownloadState::default()
    };

    // Ensure cache dir exists
    std::fs::create_dir_all(&config.server.cache_dir).unwrap_or_else(|e| {
        eprintln!("failed to create cache dir: {}", e);
        std::process::exit(1);
    });

    let bind_addr = config.server.bind;
    let use_tls = config.server.tls_cert.is_some() && config.server.tls_key.is_some();

    let state = Arc::new(AppState {
        config,
        downloads: RwLock::new(downloads),
        thumb_semaphore: Semaphore::new(1),
    });

    let app = web::router(state.clone());

    if use_tls {
        let tls_config = load_tls_config(
            state.config.server.tls_cert.as_ref().unwrap(),
            state.config.server.tls_key.as_ref().unwrap(),
        )
        .unwrap_or_else(|e| {
            eprintln!("failed to load TLS config: {}", e);
            std::process::exit(1);
        });

        let tls_acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config));
        let listener = tokio::net::TcpListener::bind(bind_addr).await.unwrap_or_else(|e| {
            eprintln!("failed to bind to {}: {}", bind_addr, e);
            std::process::exit(1);
        });

        eprintln!("listening on https://{}", bind_addr);

        loop {
            let (stream, _addr) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("accept error: {}", e);
                    continue;
                }
            };

            let acceptor = tls_acceptor.clone();
            let app = app.clone();

            tokio::spawn(async move {
                let Ok(tls_stream) = acceptor.accept(stream).await else {
                    return;
                };

                let io = hyper_util::rt::TokioIo::new(tls_stream);
                let service = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let mut app = app.clone();
                    async move {
                        use tower::Service;
                        app.call(req.map(axum::body::Body::new)).await
                    }
                });

                hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await
                    .ok();
            });
        }
    } else {
        let listener = tokio::net::TcpListener::bind(bind_addr).await.unwrap_or_else(|e| {
            eprintln!("failed to bind to {}: {}", bind_addr, e);
            std::process::exit(1);
        });

        eprintln!("listening on http://{}", bind_addr);
        axum::serve(listener, app).await.unwrap();
    }
}
