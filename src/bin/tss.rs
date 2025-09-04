use anyhow::Result;
use std::sync::Arc;
use tigris_security::{config::Settings, AppContext, run};

#[tokio::main]
async fn main() -> Result<()> {
    let settings = Settings::load()?;
    let ctx: Arc<AppContext> = AppContext::bootstrap(settings).await?;
    run(ctx).await
}
