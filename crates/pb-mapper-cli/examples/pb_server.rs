use pb_mapper_core::config::init_tracing;
use pb_mapper_server::run_server;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    init_tracing();
    run_server("[::1]:7666").await
}
