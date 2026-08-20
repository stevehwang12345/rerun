use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let address = "127.0.0.1:8080";
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("RMS server listening on http://{address}");
    axum::serve(listener, rms_server::fixture_router()).await?;
    Ok(())
}
