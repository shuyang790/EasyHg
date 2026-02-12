mod app;
mod config;
mod domain;
mod hg;
mod ui;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let config = config::load_config();
    app::run_app(config).await
}
