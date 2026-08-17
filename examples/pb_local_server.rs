use pb_mapper::common::config::init_tracing;
use pb_mapper::local::server::{run_server_side_cli, ServerTunnelOptions};
use uni_stream::stream::TcpStreamProvider;

#[tokio::main]
async fn main() {
    init_tracing();
    run_server_side_cli::<TcpStreamProvider, _>(
        "[::1]:11111",
        "[::1]:7666",
        "echo".into(),
        ServerTunnelOptions {
            need_codec: false,
            is_datagram: false,
            keep_alive: false,
            namespace: None,
            force_namespace: false,
        },
    )
    .await;
}
