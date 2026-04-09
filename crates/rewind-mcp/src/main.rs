use anyhow::Result;
use rewind_store::Store;
use rmcp::ServiceExt;

mod server;

#[tokio::main]
async fn main() -> Result<()> {
    // Log to stderr — stdout is the MCP JSON-RPC transport
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rewind_mcp=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();

    let store = Store::open_default()?;
    let server = server::RewindMcp::new(store);

    tracing::info!("rewind-mcp server starting on stdio");

    let service = server
        .serve(rmcp::transport::io::stdio())
        .await
        .inspect_err(|e| tracing::error!("serve error: {e}"))?;

    service.waiting().await?;
    Ok(())
}
