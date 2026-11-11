mod auth;
mod desktop;
mod encoding;
mod http3;
mod protocol;
mod resize;
mod server;
mod transport;
mod worker;

use clap::Parser;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::{Semaphore, watch};

#[derive(Parser)]
#[command(version, about = "Rust desktop server for browser remote control")]
struct Args {
    #[arg(long, env = "RWD_HOST", default_value = "127.0.0.1")]
    host: std::net::IpAddr,
    #[arg(long, env = "RWD_PORT", default_value_t = 8080)]
    port: u16,
    /// Enable HTTP/3 WebTransport on this UDP port (WebSocket remains available).
    #[arg(long, env = "RWD_HTTP3_PORT")]
    http3_port: Option<u16>,
    /// HTTP/3 UDP bind address; defaults to --host. Useful with a local HTTPS proxy.
    #[arg(long, env = "RWD_HTTP3_HOST", requires = "http3_port")]
    http3_host: Option<std::net::IpAddr>,
    /// Synthetic desktop; never captures or controls the real host.
    #[arg(long)]
    demo: bool,
    /// Request missing macOS permissions (already automatic for native server startup).
    #[arg(long, conflicts_with_all = ["no_permission_prompts", "demo", "benchmark"])]
    request_permissions: bool,
    /// Disable automatic permission prompts and opening System Settings on startup.
    #[arg(long)]
    no_permission_prompts: bool,
    /// Exact external origin when using a reverse proxy (e.g. https://remote.example.com).
    #[arg(long, env = "RWD_PUBLIC_ORIGIN")]
    public_origin: Option<String>,
    /// Send cookies only over HTTPS (enable behind a TLS reverse proxy).
    #[arg(long)]
    secure_cookie: bool,
    /// Initial encoder. Browser may negotiate a different codec.
    #[arg(long, value_enum, default_value = "jpeg")]
    codec: encoding::Codec,
    /// Existing resampling library: SIMD implementation or image baseline.
    #[arg(long, value_enum, default_value = "fast")]
    resize: resize::ResizeMethod,
    /// Native capture library. Auto prefers ScreenCaptureKit on macOS.
    #[arg(long, env = "RWD_CAPTURE_BACKEND", value_enum, default_value = "auto")]
    capture: desktop::CaptureBackend,
    /// OpenH264 real-time usage mode; screen preserves the original baseline.
    #[arg(long, env = "RWD_H264_MODE", value_enum, default_value = "video")]
    h264_mode: encoding::H264Mode,
    /// Compare library encoders on an identical synthetic workload; no desktop access.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=10000))]
    benchmark: Option<u32>,
}

impl Args {
    fn should_request_permissions(&self) -> bool {
        !self.demo
            && self.benchmark.is_none()
            && !self.no_permission_prompts
            && (cfg!(target_os = "macos") || self.request_permissions)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "remote_web_desktop=info".into()),
        )
        .init();
    let args = Args::parse();
    if let Some(iterations) = args.benchmark {
        encoding::benchmark(iterations)?;
        return resize::benchmark(iterations).map_err(Into::into);
    }
    if let Some(origin) = &args.public_origin {
        let uri: axum::http::Uri = origin.parse()?;
        if !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri.authority().is_none()
            || uri
                .path_and_query()
                .is_some_and(|part| part.as_str() != "/")
            || origin.ends_with('/')
            || origin.contains('@')
        {
            return Err(
                "--public-origin must be an exact http(s) origin without path or trailing slash"
                    .into(),
            );
        }
    }
    if args.secure_cookie
        && !args
            .public_origin
            .as_deref()
            .is_some_and(|origin| origin.starts_with("https://"))
    {
        return Err(
            "--secure-cookie requires --public-origin https://... behind a TLS reverse proxy"
                .into(),
        );
    }
    let password = match std::env::var("RWD_PASSWORD") {
        Ok(value) if !value.is_empty() && value.len() <= 1024 => value,
        Ok(_) => return Err("RWD_PASSWORD must contain 1–1024 UTF-8 bytes".into()),
        Err(std::env::VarError::NotPresent) => {
            let value = auth::random_token();
            eprintln!("\n접속 비밀번호 (이번 실행에서만 유효): {value}\n");
            value
        }
        Err(error) => return Err(error.into()),
    };
    let addr = SocketAddr::new(args.host, args.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let http3 = args
        .http3_port
        .map(|port| http3::Http3::bind(SocketAddr::new(args.http3_host.unwrap_or(args.host), port)))
        .transpose()?;
    // Ask locally once, after startup validation, never in response to a client.
    // Missing grants or an unavailable GUI must not prevent serving the web UI.
    if args.should_request_permissions()
        && let Err(error) = desktop::request_permissions()
    {
        tracing::warn!(%error, "Could not request OS permissions; server will continue");
    }
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let state = Arc::new(server::AppState {
        auth: auth::Auth::new(&password),
        public_origin: args.public_origin,
        secure_cookie: args.secure_cookie,
        demo: args.demo,
        initial_codec: args.codec,
        resize_method: args.resize,
        capture_backend: args.capture,
        h264_mode: args.h264_mode,
        controller: Arc::new(Semaphore::new(1)),
        shutdown: shutdown_rx,
        http3: http3.clone(),
    });
    drop(password);
    let http3_task = http3.map(|server| tokio::spawn(server.serve(state.clone())));
    tracing::info!(address = %addr, demo = args.demo, "Remote desktop server ready");
    eprintln!("브라우저 접속: http://{addr}  (종료: Ctrl+C)");
    let shutdown_signal = shutdown_tx.clone();
    let result = axum::serve(
        listener,
        server::router(state.clone()).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        termination_signal().await;
        let _ = shutdown_signal.send(true);
    })
    .await;
    let _ = shutdown_tx.send(true);
    if let Some(task) = http3_task {
        let _ = task.await;
    }
    // Keep the runtime alive until the native worker releases all held inputs.
    let _released = state.controller.acquire().await?;
    result?;
    Ok(())
}

async fn termination_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            },
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_startup_requests_permissions_by_default_on_macos() {
        let args = Args::try_parse_from(["rwd"]).unwrap();
        assert_eq!(args.should_request_permissions(), cfg!(target_os = "macos"));
        let args = Args::try_parse_from(["rwd", "--request-permissions"]).unwrap();
        assert!(args.should_request_permissions());
    }

    #[test]
    fn synthetic_modes_and_opt_out_never_request_permissions() {
        for options in [
            vec!["--demo"],
            vec!["--benchmark", "1"],
            vec!["--no-permission-prompts"],
        ] {
            let args = Args::try_parse_from(std::iter::once("rwd").chain(options)).unwrap();
            assert!(!args.should_request_permissions());
        }
    }

    #[test]
    fn explicit_permission_requests_cannot_override_synthetic_modes_or_opt_out() {
        for options in [
            vec!["--demo"],
            vec!["--benchmark", "1"],
            vec!["--no-permission-prompts"],
        ] {
            let error =
                Args::try_parse_from(["rwd", "--request-permissions"].into_iter().chain(options))
                    .err()
                    .expect("conflicting permission options must be rejected");
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }
}
