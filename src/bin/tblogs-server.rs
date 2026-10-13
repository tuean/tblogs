//! `tblogs-server` — serve a published content package over HTTP.
//!
//! Read-only by design: it has no authoring interface and never writes to the
//! database. Authoring happens locally with `tblogs-admin`.

use anyhow::Result;
use clap::Parser;
use tblogs::{
    config::{ServerArgs, ServerConfig},
    server,
};

#[tokio::main]
async fn main() -> Result<()> {
    server::init_tracing();

    let config = ServerConfig::from_args(ServerArgs::parse());
    server::run(config).await
}
