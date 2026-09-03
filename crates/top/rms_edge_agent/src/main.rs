//! RMS edge-agent executable.

use std::{env, path::PathBuf, process::ExitCode};

use rms_edge_agent::{ConfigStore, EdgeAgent};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .try_init()
        .ok();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %error, "RMS edge agent stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = config_path()?;
    let config = ConfigStore::new(config_path).load()?;
    let agent = EdgeAgent::from_config(config)?;
    let cancellation = CancellationToken::new();
    let signal_cancellation = cancellation.clone();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        signal_cancellation.cancel();
    });
    agent.run(cancellation).await?;
    Ok(())
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let terminate = signal(SignalKind::terminate());
    match terminate {
        Ok(mut terminate) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
        }
        Err(_) => {
            drop(tokio::signal::ctrl_c().await);
        }
    }
}

#[cfg(windows)]
async fn wait_for_shutdown_signal() {
    use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_shutdown};

    let (close, shutdown, ctrl_break) = (ctrl_close(), ctrl_shutdown(), ctrl_break());
    match (close, shutdown, ctrl_break) {
        (Ok(mut close), Ok(mut shutdown), Ok(mut ctrl_break)) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = close.recv() => {},
                _ = shutdown.recv() => {},
                _ = ctrl_break.recv() => {},
            }
        }
        _ => {
            drop(tokio::signal::ctrl_c().await);
        }
    }
}

#[cfg(not(any(unix, windows)))]
async fn wait_for_shutdown_signal() {
    drop(tokio::signal::ctrl_c().await);
}

fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let first = arguments.next();
    let path = match first.as_deref() {
        Some(value) if value == "--config" => arguments.next().ok_or("--config requires a path")?,
        None => env::var_os("RMS_EDGE_CONFIG")
            .ok_or("configuration is required: pass --config <path> or set RMS_EDGE_CONFIG")?,
        _ => return Err("usage: rms-edge-agent --config <path>".into()),
    };
    if arguments.next().is_some() {
        return Err("usage: rms-edge-agent --config <path>".into());
    }
    Ok(PathBuf::from(path))
}
