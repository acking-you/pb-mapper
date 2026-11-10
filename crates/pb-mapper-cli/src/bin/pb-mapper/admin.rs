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

use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use pb_mapper_auth::{
    KeyPage, discard_staged_admin_key, generate_admin_key, stage_admin_key_candidate,
    write_admin_key_file,
};
use pb_mapper_core::checksum::set_process_msg_header_key;
use pb_mapper_core::config::pb_mapper_server_addr;
use pb_mapper_protocol::command::{
    ACTION_ADMINISTRATOR_KEY_ROTATED, AdminConnectionPage, AdminRequest, AdminResponse,
    AdminServicePage,
};

use crate::parse_duration;
use crate::server::LegacyProtocolArg;

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
    /// Reserve namespaces that several temporary keys can share.
    Namespace(AdminNamespaceArgs),
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
        /// Issue into a namespace from `admin namespace allocate`. Omit for a key
        /// that owns a namespace of its own.
        #[arg(long)]
        namespace: Option<u64>,
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
    command: AdminConnectionCommand,
}

#[derive(Debug, Args)]
struct AdminServiceArgs {
    #[command(subcommand)]
    command: AdminServiceCommand,
}

/// The paging flags every inventory listing takes, declared once.
#[derive(Debug, Clone, Args)]
struct AdminListArgs {
    /// List only this namespace. A key issued into a namespace has a different id, so
    /// its id here lists nothing.
    #[arg(long, visible_alias = "key-id")]
    namespace: Option<u64>,
    #[arg(long, default_value_t = 0)]
    page: u32,
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
    page_size: u16,
    #[arg(long, default_value_t = false)]
    all: bool,
}

#[derive(Debug, Clone, Subcommand)]
enum AdminConnectionCommand {
    /// List relay connections across namespaces.
    List(AdminListArgs),
    /// Drop registered control connections the relay is still holding.
    ///
    /// For a registration the relay cannot see is gone: one that still answers
    /// its heartbeat but no longer forwards, or a service whose connection quota
    /// is full of connections that should have unwound. A healthy client
    /// reconnects afterwards, so this costs a brief interruption at worst.
    ///
    /// The group makes the target mandatory and exclusive, so the error for
    /// naming neither lists both ways to name one.
    #[command(group = clap::ArgGroup::new("target").required(true))]
    Retire {
        /// Service to retire connections for, unscoped by the namespace prefix.
        service_name: String,
        /// Namespace owning the service. Omit for the unscoped namespace.
        #[arg(long, visible_alias = "key-id")]
        namespace: Option<u64>,
        /// Retire just this connection, as reported by `admin connection list`.
        #[arg(long, group = "target")]
        conn_id: Option<u32>,
        /// Retire every connection the service has. Required to be explicit,
        /// because it is what frees a full quota and also what interrupts every
        /// healthy connection at once.
        #[arg(long, group = "target", default_value_t = false)]
        all: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum AdminServiceCommand {
    /// List registered services across namespaces.
    List(AdminListArgs),
}

#[derive(Debug, Args)]
struct AdminNamespaceArgs {
    #[command(subcommand)]
    command: AdminNamespaceCommand,
}

#[derive(Debug, Subcommand)]
enum AdminNamespaceCommand {
    /// Reserve a namespace. Its number is never handed out again.
    Allocate,
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
    let output = args.output;
    // Listings and the root-key rotation run their own exchanges; every other
    // command is one request whose response is printed as it comes.
    let request = match args.command {
        AdminCommand::Namespace(AdminNamespaceArgs {
            command: AdminNamespaceCommand::Allocate,
        }) => AdminRequest::NamespaceAllocate,
        AdminCommand::Key(AdminKeyArgs { command }) => match command {
            AdminKeyCommand::Issue {
                ttl,
                label,
                namespace,
            } => {
                let ttl_seconds = ttl.as_secs();
                match namespace {
                    Some(namespace) => AdminRequest::KeyIssueScoped {
                        namespace,
                        ttl_seconds,
                        label,
                    },
                    None => AdminRequest::KeyIssue { ttl_seconds, label },
                }
            }
            AdminKeyCommand::List {
                page,
                page_size,
                all,
            } => {
                return stream_pages::<KeyPage, _>(remote_addr, output, page, all, |page| {
                    AdminRequest::KeyList { page, page_size }
                })
                .await;
            }
            AdminKeyCommand::Show { key_id } => AdminRequest::KeyShow { key_id },
            AdminKeyCommand::Reveal { key_id } => AdminRequest::KeyReveal { key_id },
            AdminKeyCommand::Renew { key_id, ttl } => AdminRequest::KeyRenew {
                key_id,
                ttl_seconds: ttl.as_secs(),
            },
            AdminKeyCommand::Revoke { key_id } => AdminRequest::KeyRevoke { key_id },
            AdminKeyCommand::Gc => AdminRequest::KeyGc,
        },
        AdminCommand::Connection(AdminConnectionArgs { command }) => match command {
            AdminConnectionCommand::List(AdminListArgs {
                namespace,
                page,
                page_size,
                all,
            }) => {
                return stream_pages::<AdminConnectionPage, _>(
                    remote_addr,
                    output,
                    page,
                    all,
                    |page| AdminRequest::ConnectionList {
                        key_id: namespace,
                        page,
                        page_size,
                    },
                )
                .await;
            }
            AdminConnectionCommand::Retire {
                service_name,
                namespace,
                conn_id,
                all,
            } => {
                // `--all` is the absence of a target, not a separate field on the
                // wire: clap has already ensured exactly one of the two was given.
                let conn_id = if all { None } else { conn_id };
                AdminRequest::ConnectionRetire {
                    key_id: namespace,
                    service_name,
                    conn_id,
                }
            }
        },
        AdminCommand::Service(AdminServiceArgs {
            command:
                AdminServiceCommand::List(AdminListArgs {
                    namespace,
                    page,
                    page_size,
                    all,
                }),
        }) => {
            return stream_pages::<AdminServicePage, _>(remote_addr, output, page, all, |page| {
                AdminRequest::ServiceList {
                    key_id: namespace,
                    page,
                    page_size,
                }
            })
            .await;
        }
        AdminCommand::Status => AdminRequest::AuthStatus,
        AdminCommand::AuthState(AdminAuthStateArgs {
            command: AdminAuthStateCommand::Reset { confirm },
        }) => AdminRequest::AuthStateReset { confirm },
        AdminCommand::RootKey(AdminRootKeyArgs {
            command: AdminRootKeyCommand::Rotate { new_key, key_file },
        }) => return rotate_root_key(remote_addr, output, new_key, key_file).await,
        AdminCommand::LegacyProtocol(AdminLegacyProtocolArgs {
            command: AdminLegacyProtocolCommand::Set { policy },
        }) => AdminRequest::LegacyProtocolSet {
            policy: policy.into(),
        },
    };
    let response = send_admin_request(remote_addr, request).await?;
    print_admin_response(output, &response)
}

/// Rotate the sole administrator key, in an order that never leaves the new
/// key without a copy on disk: stage it, ask the relay, switch this process
/// to it, verify it, write the key file, and only then discard the staged
/// copy.
async fn rotate_root_key(
    remote_addr: &str,
    output: OutputFormat,
    new_key: Option<String>,
    key_file: Option<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    let key_file = match key_file {
        Some(key_file) => key_file,
        None => default_admin_recovery_key_file()?,
    };
    let new_key = new_key.unwrap_or_else(generate_admin_key);
    // Staged before the relay is contacted, and never over an unresolved
    // candidate from an earlier attempt: that file may hold the only copy
    // of a key the relay already installed.
    let staged_key_file = stage_admin_key_candidate(&key_file, &new_key)?;
    let client = admin_client(remote_addr)?;
    let response = match client.admin()?.rotate_root_key(Some(new_key.clone())).await {
        Ok(_) => AdminResponse::Ok {
            action: ACTION_ADMINISTRATOR_KEY_ROTATED.to_string(),
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
    if output == OutputFormat::Human {
        println!("administrator key rotated and verified");
        println!("all temporary credentials are now invalid (temporary_key_rotated)");
        println!("key file: {}", key_file.display());
    } else {
        print_admin_response(output, &response)?;
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
trait AdminPage {
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
///
/// `empty_clone` and `absorb` name every field of the page, so a field it
/// gains needs a decision here before `--all` compiles.
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
                let Self {
                    schema_version,
                    items: _,
                    next_page: _,
                } = self;
                Self {
                    schema_version: *schema_version,
                    items: Vec::new(),
                    next_page: None,
                }
            }

            fn absorb(&mut self, page: &Self) {
                let Self {
                    schema_version: _,
                    items,
                    next_page: _,
                } = page;
                self.items.extend_from_slice(items);
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
            println!("namespace: {}", key.metadata.effective_namespace());
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
            println!("KEY ID\tNAMESPACE\tSTATE\tEXPIRES\tLABEL");
            for key in &page.items {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    key.key_id,
                    key.effective_namespace(),
                    key.state,
                    key.expires_at,
                    key.label.as_deref().unwrap_or("")
                );
            }
            if let Some(next) = page.next_page {
                println!("next page: {next}");
            }
        }
        AdminResponse::NamespaceAllocated { namespace } => println!("namespace: {namespace}"),
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
            println!("capabilities: {}", status.capabilities.join(", "));
        }
        AdminResponse::Services(page) => {
            println!("NAMESPACE\tSERVICE\tTRANSPORT\tCODEC\tCONNECTIONS");
            for service in &page.items {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    service.namespace,
                    service.service_name,
                    service.transport,
                    service.codec_enabled,
                    service.connection_count
                );
            }
            if let Some(next) = page.next_page {
                println!("next page: {next}");
            }
        }
        AdminResponse::Connections(page) => {
            // IDLE MS is what tells an operator which connection to retire: a
            // registration well past its lease is one the relay is holding for
            // nothing.
            println!("NAMESPACE\tSERVICE\tCONN ID\tHEALTHY\tIDLE MS\tTRANSPORT\tCODEC");
            for connection in &page.items {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    connection.namespace,
                    connection.service_name,
                    connection.conn_id,
                    connection.healthy,
                    connection.last_rx_age_ms,
                    connection.transport,
                    connection.codec_enabled
                );
            }
            if let Some(next) = page.next_page {
                println!("next page: {next}");
            }
        }
        AdminResponse::ConnectionsRetired { retired } => {
            println!("retired {retired} registered connections");
        }
        AdminResponse::Ok { action } => println!("ok: {action}"),
    }
}

/// Where `root-key rotate` keeps the new key when `--key-file` does not say.
fn default_admin_recovery_key_file() -> std::io::Result<PathBuf> {
    recovery_key_file_in(|name| std::env::var_os(name))
}

/// `pb-mapper/admin.key` in `$XDG_CONFIG_HOME`, or else in `.config` in the
/// home: `$HOME`, or on Windows `%USERPROFILE%`. Each must be a whole path,
/// as XDG asks. With none, the caller names the file: the directory the
/// command runs in may be anything, a shared checkout included, and is no
/// place for the relay's administrator key.
fn recovery_key_file_in(
    var: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> std::io::Result<PathBuf> {
    let whole = |name| {
        var(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let home = || {
        whole("HOME")
            .or_else(|| cfg!(windows).then(|| whole("USERPROFILE")).flatten())
            .map(|home| home.join(".config"))
    };
    let config = whole("XDG_CONFIG_HOME").or_else(home).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no home directory to keep the new administrator key in; pass --key-file",
        )
    })?;
    Ok(config.join("pb-mapper").join("admin.key"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    /// The environment `vars` names, and nothing else.
    fn only(vars: Vec<(&'static str, OsString)>) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            vars.iter()
                .find(|(var, _)| *var == name)
                .map(|(_, value)| value.clone())
        }
    }

    /// The recovery copy of a new administrator key goes to the user's
    /// configuration directory, and never into whatever directory the
    /// command happened to run in.
    #[test]
    fn the_recovery_key_lands_in_the_users_config_or_nowhere() {
        let home = std::env::temp_dir();
        let config = home.join("config");
        assert_eq!(
            recovery_key_file_in(only(vec![
                ("XDG_CONFIG_HOME", config.clone().into()),
                ("HOME", home.clone().into()),
            ]))
            .unwrap(),
            config.join("pb-mapper").join("admin.key")
        );
        let in_home = home.join(".config").join("pb-mapper").join("admin.key");
        assert_eq!(
            recovery_key_file_in(only(vec![("HOME", home.clone().into())])).unwrap(),
            in_home
        );
        // Windows keeps the home in USERPROFILE.
        let from_profile = recovery_key_file_in(only(vec![("USERPROFILE", home.into())]));
        if cfg!(windows) {
            assert_eq!(from_profile.unwrap(), in_home);
        } else {
            assert!(from_profile.is_err());
        }
        // No home, or one that is not a whole path, leaves nowhere to put it.
        assert!(recovery_key_file_in(only(Vec::new())).is_err());
        assert!(
            recovery_key_file_in(only(vec![
                ("XDG_CONFIG_HOME", "".into()),
                ("HOME", "home".into()),
            ]))
            .is_err()
        );
    }
}
