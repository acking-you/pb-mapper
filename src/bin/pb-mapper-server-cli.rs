use clap::Parser;
use mimalloc_rust::GlobalMiMalloc;
use pb_mapper::common::config::{get_pb_mapper_server, get_sockaddr, init_tracing, LocalService};
use pb_mapper::common::stream::{TcpStreamProvider, UdpStreamProvider};
use pb_mapper::local::client::handle_status_cli;
use pb_mapper::local::server::run_server_side_cli;
use pb_mapper::snafu_error_get_or_return;

#[global_allocator]
static GLOBAL_MIMALLOC: GlobalMiMalloc = GlobalMiMalloc;

#[derive(Parser)]
#[command(author = "L_B__", version, about, long_about = None)]
struct Cli {
    /// Local service that need to be exposed
    #[command(subcommand)]
    local_server: LocalService,
    /// [optional] Remote service registry, note that you need to include IP and port,such as
    /// `127.0.0.1:1080`. When it is none, we take the value of the environment
    /// variable `PB_MAPPER_SERVER`, and if that value is still null, we report an error
    #[arg(short, long, value_name = "PB_MAPPER_SERVER")]
    pb_mapper_server: Option<String>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_tracing();
    match cli.local_server {
        LocalService::UdpServer { key, addr } => {
            run_server_side_cli::<UdpStreamProvider, _>(
                snafu_error_get_or_return!(get_sockaddr(&addr)),
                snafu_error_get_or_return!(get_pb_mapper_server(cli.pb_mapper_server.as_deref())),
                key.into(),
            )
            .await
        }
        LocalService::TcpServer { key, addr } => {
            run_server_side_cli::<TcpStreamProvider, _>(
                snafu_error_get_or_return!(get_sockaddr(&addr)),
                snafu_error_get_or_return!(get_pb_mapper_server(cli.pb_mapper_server.as_deref())),
                key.into(),
            )
            .await
        }
        LocalService::Status { op } => {
            handle_status_cli(
                op,
                snafu_error_get_or_return!(get_pb_mapper_server(cli.pb_mapper_server.as_deref())),
            )
            .await
        }
    }
}
