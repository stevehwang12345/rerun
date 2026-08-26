use std::{error::Error, net::SocketAddr};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let address = std::env::var("RMS_BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse::<SocketAddr>()?;
    if !address.ip().is_loopback() {
        return Err("RMS_BIND_ADDR must use a loopback address; publish selected routes through an authenticated TLS gateway".into());
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("RMS server listening on http://{address}");
    axum::serve(listener, rms_server::durable_fixture_router()?).await?;
    Ok(())
}
