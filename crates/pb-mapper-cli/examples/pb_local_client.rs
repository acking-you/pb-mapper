use pb_mapper_client::client::run_client_side_cli;
use pb_mapper_core::config::init_tracing;
use uni_stream::stream::TcpListenerProvider;

#[tokio::main]
async fn main() {
    init_tracing();
    run_client_side_cli::<TcpListenerProvider, _>(
        "[::1]:22222",
        "[::1]:7666",
        "echo".into(),
        false,
    )
    .await;
}
