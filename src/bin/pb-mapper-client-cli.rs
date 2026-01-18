use clap::Parser;
use mimalloc_rust::GlobalMiMalloc;
use pb_mapper::common::config::{
    get_pb_mapper_server_async, get_sockaddr_async, init_tracing, LocalService,
    PB_MAPPER_KEEP_ALIVE,
};
use pb_mapper::local::client::{handle_status_cli, run_client_side_cli};
use pb_mapper::snafu_error_get_or_return;
use uni_stream::stream::{TcpListenerProvider, UdpListenerProvider};

#[global_allocator]
static GLOBAL_MIMALLOC: GlobalMiMalloc = GlobalMiMalloc;

#[derive(Parser)]
#[command(author = "L_B__", version, about, long_about = None)]
struct Cli {
    /// Service exposed for local use
    #[command(subcommand)]
    local_server: LocalService,
    /// [optional] Remote service registry, note that you need to include IP and port,such as
    /// `127.0.0.1:1080`. When it is none, we take the value of the environment
    /// variable `PB_MAPPER_SERVER`, and if that value is still null, we report an error
    #[arg(short, long, value_name = "PB_MAPPER_SERVER")]
    pb_mapper_server: Option<String>,
    /// [optional] keep-alive for local client stream. by default, it is false. Note that
    /// keep-alive is also controlled by the env:`PB_MAPPER_KEEP_ALIVE`.
    #[arg(
        short,
        long,
        value_name = "PB_MAPPER_KEEP_ALIVE",
        default_value_t = false
    )]
    keep_alive: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_tracing();
    if cli.keep_alive {
        std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
    }
    match cli.local_server {
        LocalService::UdpServer { key, addr } => {
            run_client_side_cli::<UdpListenerProvider, _>(
                snafu_error_get_or_return!(get_sockaddr_async(&addr).await),
                snafu_error_get_or_return!(
                    get_pb_mapper_server_async(cli.pb_mapper_server.as_deref()).await
                ),
                key.into(),
            )
            .await
        }
        LocalService::TcpServer { key, addr } => {
            run_client_side_cli::<TcpListenerProvider, _>(
                snafu_error_get_or_return!(get_sockaddr_async(&addr).await),
                snafu_error_get_or_return!(
                    get_pb_mapper_server_async(cli.pb_mapper_server.as_deref()).await
                ),
                key.into(),
            )
            .await;
        }
        LocalService::Status { op } => {
            handle_status_cli(
                op,
                snafu_error_get_or_return!(
                    get_pb_mapper_server_async(cli.pb_mapper_server.as_deref()).await
                ),
            )
            .await
        }
    }
}
