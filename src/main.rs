use std::net::SocketAddr;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut data = PathBuf::from("strata-data");
    let mut address: SocketAddr = "127.0.0.1:9200".parse()?;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data" => data = args.next().ok_or("--data needs a directory")?.into(),
            "--listen" => address = args.next().ok_or("--listen needs an address")?.parse()?,
            "--help" | "-h" => {
                println!(
                    "strata [--data DIRECTORY] [--listen 127.0.0.1:9200]\nLoopback only; no authentication or TLS. Writes are durable; POST /INDEX/_refresh exposes them to search."
                );
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    if !address.ip().is_loopback() {
        return Err("only loopback binds are supported; put an authenticated proxy in front for remote access".into());
    }
    let router = strata_search::router(&data)?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!(
        "strata listening on {} (data: {})",
        listener.local_addr()?,
        data.display()
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
