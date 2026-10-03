//! Loopback-only relay for scripts/test_version_compatibility.py. This example
//! uses a public test credential and must never be exposed as a public relay.

use pb_mapper_auth::AuthRuntime;
use pb_mapper_server::run_server_on_listener;
use pb_mapper_testkit::{admin_key_bytes, auth_config, init_test_env};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The harness fixes the process credential. The driver sets RUST_LOG=off
    // and recognizes the readiness line independently of any other output.
    init_test_env();
    let mut config = auth_config("compatibility-binary");
    config.state_dir = std::env::args()
        .nth(1)
        .ok_or("missing private state directory")?
        .into();
    let auth = AuthRuntime::start(admin_key_bytes(), config).await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    println!("COMPAT_RELAY={}", listener.local_addr()?);
    run_server_on_listener(listener, CancellationToken::new(), None, false, auth).await?;
    Ok(())
}
