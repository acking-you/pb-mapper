use pb_mapper::common::config::init_tracing;
use uni_stream::stream::TcpStreamProvider;
use pb_mapper::local::server::run_server_side_cli;

#[tokio::main]
async fn main() {
    init_tracing();
    run_server_side_cli::<TcpStreamProvider, _>(
        "[::1]:11111",
        "[::1]:7666",
        "echo".into(),
        false,
        false,
    )
    .await;
}
