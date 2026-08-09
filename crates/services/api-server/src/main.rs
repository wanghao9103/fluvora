mod app;
mod config;
mod control_client;
mod error;
mod gateway_client;
mod gateway_routes;
mod gift;
mod models;
mod persistence;
mod protocol;
mod routes;
mod runtime;
mod services;
mod signals;
mod transcode_reconciler;
mod validation;

#[tokio::main]
async fn main() {
    app::run().await;
}
