use pb_mapper::common::config::init_tracing;
use uni_stream::stream::TcpListenerProvider;
use pb_mapper::local::client::run_client_side_cli;

#[tokio::main]
async fn main() {
    init_tracing();
    run_client_side_cli::<TcpListenerProvider, _>("[::1]:22222", "[::1]:7666", "echo".into()).await;
}
