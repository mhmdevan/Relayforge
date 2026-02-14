#[tokio::main]
async fn main() -> anyhow::Result<()> {
    worker::run_from_env().await
}
