//! Administrator CLI: command parsing, one-shot V2 requests, pagination, and rendering.
//!
//! ```text
//! admin args -> AdminRequest -> authenticated V2 connection -> relay
//!     ^                                                    |
//!     +--- human / JSON / NDJSON <- AdminResponse <--------+
//! ```
//!
//! `--all` keeps the selected output contract: human and JSON aggregate pages,
//! while NDJSON deliberately streams one item at a time. Root-key rotation stages
//! a recovery copy before contacting the relay, then verifies the new credential.

use super::*;

#[derive(Debug, Args)]
pub(super) struct AdminArgs {
    /// Relay address. Falls back to PB_MAPPER_SERVER.
    #[arg(short, long, visible_alias = "pb-mapper-server", value_name = "ADDR")]
    server: Option<String>,
    /// Machine-readable output mode.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    output: OutputFormat,
    #[command(subcommand)]
    command: AdminCommand,
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Issue, inspect, renew, reveal, revoke, or collect temporary keys.
    Key(AdminKeyArgs),
    /// List relay connections across namespaces.
    Connection(AdminConnectionArgs),
    /// List registered services across namespaces.
    Service(AdminServiceArgs),
    /// Show authentication state and protocol counters.
    Status,
    /// Repair or reset encrypted temporary-key state.
    AuthState(AdminAuthStateArgs),
    /// Rotate the sole administrator key and invalidate every existing credential.
    RootKey(AdminRootKeyArgs),
    /// Change legacy protocol acceptance at runtime.
    LegacyProtocol(AdminLegacyProtocolArgs),
}

#[derive(Debug, Args)]
struct AdminKeyArgs {
    #[command(subcommand)]
    command: AdminKeyCommand,
}

#[derive(Debug, Subcommand)]
enum AdminKeyCommand {
    Issue {
        #[arg(long, value_parser = parse_duration)]
        ttl: Duration,
        #[arg(long)]
        label: Option<String>,
    },
    List {
        #[arg(long, default_value_t = 0)]
        page: u32,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        page_size: u16,
        #[arg(long, default_value_t = false)]
        all: bool,
    },
    Show {
        key_id: u64,
    },
    Reveal {
        key_id: u64,
    },
    Renew {
        key_id: u64,
        #[arg(long, value_parser = parse_duration)]
        ttl: Duration,
    },
    Revoke {
        key_id: u64,
    },
    Gc,
}

#[derive(Debug, Args)]
struct AdminConnectionArgs {
    #[command(subcommand)]
    command: AdminListCommand,
}

#[derive(Debug, Args)]
struct AdminServiceArgs {
    #[command(subcommand)]
    command: AdminListCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum AdminListCommand {
    List {
        #[arg(long)]
        key_id: Option<u64>,
        #[arg(long, default_value_t = 0)]
        page: u32,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        page_size: u16,
        #[arg(long, default_value_t = false)]
        all: bool,
    },
}

#[derive(Debug, Args)]
struct AdminAuthStateArgs {
    #[command(subcommand)]
    command: AdminAuthStateCommand,
}

#[derive(Debug, Subcommand)]
enum AdminAuthStateCommand {
    Reset {
        #[arg(long, default_value_t = false)]
        confirm: bool,
    },
}

#[derive(Debug, Args)]
struct AdminRootKeyArgs {
    #[command(subcommand)]
    command: AdminRootKeyCommand,
}

#[derive(Debug, Subcommand)]
enum AdminRootKeyCommand {
    Rotate {
        /// New 32-byte administrator key. A cryptographically random printable key is generated when omitted.
        #[arg(long)]
        new_key: Option<String>,
        /// Save the new key here before asking the relay to rotate.
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
struct AdminLegacyProtocolArgs {
    #[command(subcommand)]
    command: AdminLegacyProtocolCommand,
}

#[derive(Debug, Subcommand)]
enum AdminLegacyProtocolCommand {
    Set { policy: LegacyProtocolArg },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
    Ndjson,
}

pub(super) async fn run_admin(args: AdminArgs) -> Result<(), Box<dyn Error>> {
    // Unresolved: the SDK client resolves the name itself, and keeps every
    // address it names rather than the first.
    let remote_addr = pb_mapper_server_addr(args.server.as_deref())?;
    let remote_addr = remote_addr.as_str();
    match args.command {
        AdminCommand::Key(AdminKeyArgs { command }) => match command {
            AdminKeyCommand::Issue { ttl, label } => {
                let response = send_admin_request(
                    remote_addr,
                    AdminRequest::KeyIssue {
                        ttl_seconds: ttl.as_secs(),
                        label,
                    },
                )
                .await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::List {
                page,
                page_size,
                all,
            } => {
                stream_pages::<KeyPage, _>(remote_addr, args.output, page, all, |page| {
                    AdminRequest::KeyList { page, page_size }
                })
                .await?;
            }
            AdminKeyCommand::Show { key_id } => {
                let response =
                    send_admin_request(remote_addr, AdminRequest::KeyShow { key_id }).await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::Reveal { key_id } => {
                let response =
                    send_admin_request(remote_addr, AdminRequest::KeyReveal { key_id }).await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::Renew { key_id, ttl } => {
                let response = send_admin_request(
                    remote_addr,
                    AdminRequest::KeyRenew {
                        key_id,
                        ttl_seconds: ttl.as_secs(),
                    },
                )
                .await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::Revoke { key_id } => {
                let response =
                    send_admin_request(remote_addr, AdminRequest::KeyRevoke { key_id }).await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::Gc => {
                let response = send_admin_request(remote_addr, AdminRequest::KeyGc).await?;
                print_admin_response(args.output, &response)?;
            }
        },
        AdminCommand::Connection(AdminConnectionArgs { command }) => {
            let AdminListCommand::List {
                key_id,
                page,
                page_size,
                all,
            } = command;
            stream_pages::<AdminConnectionPage, _>(remote_addr, args.output, page, all, |page| {
                AdminRequest::ConnectionList {
                    key_id,
                    page,
                    page_size,
                }
            })
            .await?;
        }
        AdminCommand::Service(AdminServiceArgs { command }) => {
            let AdminListCommand::List {
                key_id,
                page,
                page_size,
                all,
            } = command;
            stream_pages::<AdminServicePage, _>(remote_addr, args.output, page, all, |page| {
                AdminRequest::ServiceList {
                    key_id,
                    page,
                    page_size,
                }
            })
            .await?;
        }
        AdminCommand::Status => {
            let response = send_admin_request(remote_addr, AdminRequest::AuthStatus).await?;
            print_admin_response(args.output, &response)?;
        }
        AdminCommand::AuthState(AdminAuthStateArgs {
            command: AdminAuthStateCommand::Reset { confirm },
        }) => {
            let response =
                send_admin_request(remote_addr, AdminRequest::AuthStateReset { confirm }).await?;
            print_admin_response(args.output, &response)?;
        }
        AdminCommand::RootKey(AdminRootKeyArgs {
            command: AdminRootKeyCommand::Rotate { new_key, key_file },
        }) => {
            let key_file = key_file.unwrap_or_else(default_admin_recovery_key_file);
            let new_key = new_key.unwrap_or_else(generate_admin_key);
            // Staged before the relay is contacted, and never over an unresolved
            // candidate from an earlier attempt: that file may hold the only copy
            // of a key the relay already installed.
            let staged_key_file = stage_admin_key_candidate(&key_file, &new_key)?;
            let client = admin_client(remote_addr)?;
            let response = match client.admin()?.rotate_root_key(Some(new_key.clone())).await {
                Ok(_) => AdminResponse::Ok {
                    action: "administrator_key_rotated".to_string(),
                },
                Err(error) => {
                    return Err(std::io::Error::other(format!(
                        "root rotation request failed; the candidate key remains at `{}`: {error}",
                        staged_key_file.display()
                    ))
                    .into());
                }
            };
            set_process_msg_header_key(Some(&new_key))?;
            client.admin()?.auth_status().await.map_err(|error| {
                std::io::Error::other(format!(
                    "new administrator key did not pass the post-rotation status check: {error}"
                ))
            })?;
            write_admin_key_file(&key_file, &new_key, true).map_err(|error| {
                std::io::Error::other(format!(
                    "administrator key rotated and verified, but `{}` could not be updated; recover the key from `{}`: {error}",
                    key_file.display(),
                    staged_key_file.display()
                ))
            })?;
            discard_staged_admin_key(&staged_key_file);
            if args.output == OutputFormat::Human {
                println!("administrator key rotated and verified");
                println!("all temporary credentials are now invalid (temporary_key_rotated)");
                println!("key file: {}", key_file.display());
            } else {
                print_admin_response(args.output, &response)?;
            }
        }
        AdminCommand::LegacyProtocol(AdminLegacyProtocolArgs {
            command: AdminLegacyProtocolCommand::Set { policy },
        }) => {
            let response = send_admin_request(
                remote_addr,
                AdminRequest::LegacyProtocolSet {
                    policy: policy.into(),
                },
            )
            .await?;
            print_admin_response(args.output, &response)?;
        }
    }
    Ok(())
}

fn admin_client(remote_addr: &str) -> Result<pb_mapper_client::sdk::Client, Box<dyn Error>> {
    let credential =
        pb_mapper_core::checksum::get_process_credential().map_err(std::io::Error::other)?;
    Ok(pb_mapper_client::sdk::Client::from_credential(
        remote_addr.to_string(),
        credential,
        false,
        None,
    ))
}

async fn send_admin_request(
    remote_addr: &str,
    request: AdminRequest,
) -> Result<AdminResponse, Box<dyn Error>> {
    let client = admin_client(remote_addr)?;
    Ok(client.admin()?.request(request).await?)
}

/// One paginated administrator listing, as [`stream_pages`] drives it.
///
/// Key, service, and connection listings are the same page shape three times
/// over — a schema version, a vector of items, and a `next_page` cursor that is
/// `None` on the last page. This trait is what lets one loop serve all three.
///
/// The item type stays out of the trait: the per-page methods below are all the
/// loop needs, and keeping them concrete means this crate never has to name the
/// three item types or carry a `serde` dependency for the bound.
trait AdminPage: Clone {
    /// Names this listing in the error raised when the relay answers with some
    /// other response variant.
    const LABEL: &'static str;

    /// Borrow this listing's page out of a response, or `None` if the relay
    /// answered with a different variant.
    fn from_response(response: &AdminResponse) -> Option<&Self>;

    /// Re-wrap an aggregated page for [`print_admin_response`].
    fn into_response(self) -> AdminResponse;

    fn next_page(&self) -> Option<u32>;

    /// Print one item per line, as `--output ndjson` streams them.
    fn print_ndjson(&self) -> Result<(), Box<dyn Error>>;

    /// This page with its items dropped and its cursor cleared: the accumulator
    /// `--all` fills, which keeps the relay's schema version but must not claim
    /// there is a further page.
    fn empty_clone(&self) -> Self;

    /// Append `page`'s items to this accumulator.
    fn absorb(&mut self, page: &Self);
}

/// Declares [`AdminPage`] for one wire page type.
macro_rules! impl_admin_page {
    ($page:ty, variant = $variant:ident, label = $label:literal) => {
        impl AdminPage for $page {
            const LABEL: &'static str = $label;

            fn from_response(response: &AdminResponse) -> Option<&Self> {
                match response {
                    AdminResponse::$variant(page) => Some(page),
                    _ => None,
                }
            }

            fn into_response(self) -> AdminResponse {
                AdminResponse::$variant(self)
            }

            fn next_page(&self) -> Option<u32> {
                self.next_page
            }

            fn print_ndjson(&self) -> Result<(), Box<dyn Error>> {
                for item in &self.items {
                    println!("{}", serde_json::to_string(item)?);
                }
                Ok(())
            }

            fn empty_clone(&self) -> Self {
                Self {
                    items: Vec::new(),
                    next_page: None,
                    ..self.clone()
                }
            }

            fn absorb(&mut self, page: &Self) {
                self.items.extend_from_slice(&page.items);
            }
        }
    };
}

impl_admin_page!(KeyPage, variant = KeyList, label = "key-list");
impl_admin_page!(AdminServicePage, variant = Services, label = "service-list");
impl_admin_page!(
    AdminConnectionPage,
    variant = Connections,
    label = "connection-list"
);

/// Fetch and render one page, or every page from `page` onwards when `all`.
///
/// `request` builds the listing's request for a given page number; everything
/// else — the walk, the output contract, and the aggregation — is the same for
/// all three listings.
///
/// The output modes deliberately differ under `--all`: NDJSON streams each item
/// as it arrives, while human and JSON aggregate into a single page so their
/// output stays one well-formed document.
async fn stream_pages<P, F>(
    remote_addr: &str,
    output: OutputFormat,
    mut page: u32,
    all: bool,
    request: F,
) -> Result<(), Box<dyn Error>>
where
    P: AdminPage,
    F: Fn(u32) -> AdminRequest,
{
    let mut aggregated: Option<P> = None;
    loop {
        let response = send_admin_request(remote_addr, request(page)).await?;
        let Some(current) = P::from_response(&response) else {
            return Err(std::io::Error::other(format!("unexpected {} response", P::LABEL)).into());
        };

        match (all, output) {
            (false, _) => print_admin_response(output, &response)?,
            (true, OutputFormat::Ndjson) => current.print_ndjson()?,
            (true, _) => aggregated
                .get_or_insert_with(|| current.empty_clone())
                .absorb(current),
        }

        match current.next_page() {
            Some(next_page) if all => page = next_page,
            _ => break,
        }
    }

    if let Some(aggregated) = aggregated {
        print_admin_response(output, &aggregated.into_response())?;
    }
    Ok(())
}

fn print_admin_response(
    output: OutputFormat,
    response: &AdminResponse,
) -> Result<(), Box<dyn Error>> {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "data": response,
            }))?
        ),
        OutputFormat::Ndjson => println!("{}", serde_json::to_string(response)?),
        OutputFormat::Human => print_human_admin_response(response),
    }
    Ok(())
}

fn print_human_admin_response(response: &AdminResponse) {
    match response {
        AdminResponse::KeyIssued(key)
        | AdminResponse::KeyShown(key)
        | AdminResponse::KeyRenewed(key) => {
            println!("key id: {}", key.metadata.key_id);
            println!("state: {}", key.metadata.state);
            println!("expires at: {}", key.metadata.expires_at);
            if let Some(label) = &key.metadata.label {
                println!("label: {label}");
            }
            if !key.credential.is_empty() {
                println!("credential: {}", key.credential);
            }
        }
        AdminResponse::KeyRevoked(key) => {
            println!("key {}: {}", key.key_id, key.state);
        }
        AdminResponse::KeyList(page) => {
            println!("KEY ID\tSTATE\tEXPIRES\tLABEL");
            for key in &page.items {
                println!(
                    "{}\t{}\t{}\t{}",
                    key.key_id,
                    key.state,
                    key.expires_at,
                    key.label.as_deref().unwrap_or("")
                );
            }
            if let Some(next) = page.next_page {
                println!("next page: {next}");
            }
        }
        AdminResponse::KeyGc { removed } => println!("removed {removed} inactive keys"),
        AdminResponse::AuthStatus(status) => {
            println!("safe mode: {}", status.safe_mode);
            println!(
                "keys: {} active / {} expired / {} revoked / {} capacity",
                status.active_keys, status.expired_keys, status.revoked_keys, status.capacity
            );
            println!("legacy protocol: {:?}", status.legacy_protocol);
            println!(
                "active legacy connections: {}",
                status.active_legacy_connections
            );
            println!(
                "last legacy connection: {}",
                status
                    .last_legacy_connection_at
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "never".to_string())
            );
            println!(
                "authentication: {} succeeded / {} failed",
                status.auth_successes, status.auth_failures
            );
            println!("server instance: {}", status.server_instance_id);
        }
        AdminResponse::Services(page) => {
            println!("KEY ID\tSERVICE\tTRANSPORT\tCODEC\tCONNECTIONS");
            for service in &page.items {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    service.key_id,
                    service.service_name,
                    service.transport,
                    service.codec_enabled,
                    service.connection_count
                );
            }
        }
        AdminResponse::Connections(page) => {
            println!("KEY ID\tSERVICE\tCONN ID\tHEALTHY\tTRANSPORT\tCODEC");
            for connection in &page.items {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    connection.key_id,
                    connection.service_name,
                    connection.conn_id,
                    connection.healthy,
                    connection.transport,
                    connection.codec_enabled
                );
            }
        }
        AdminResponse::Ok { action } => println!("ok: {action}"),
    }
}

fn default_admin_recovery_key_file() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pb-mapper")
        .join("admin.key")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn administrator_request_times_out_when_peer_stalls() {
        set_process_msg_header_key(Some("0123456789abcdefghijklmnopqrstuv"))
            .expect("test administrator credential should be valid");
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("test listener should bind");
        let remote_addr = listener
            .local_addr()
            .expect("listener should have an address");
        let stalled_peer = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.expect("test peer should connect");
            std::future::pending::<()>().await;
        });

        let error = admin_client(&remote_addr.to_string())
            .expect("test client")
            .admin()
            .expect("administrator credential")
            .request_with_timeout(AdminRequest::AuthStatus, Duration::from_millis(50))
            .await
            .expect_err("a stalled administrator request should time out");

        assert!(
            matches!(error, pb_mapper_client::sdk::Error::TimedOut { .. }),
            "timeout should be reported as TimedOut, got {error}"
        );
        stalled_peer.abort();
    }
}
