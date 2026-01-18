use std::future::{self, Future};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::pin::Pin;
use std::sync::LazyLock;
use std::task::{ready, Context, Poll};

use tokio::task::JoinHandle;
use trust_dns_resolver::config::{NameServerConfigGroup, ResolverConfig, ResolverOpts};
use trust_dns_resolver::{Resolver, TokioAsyncResolver};

type Result<T, E = std::io::Error> = std::result::Result<T, E>;
type ReadyFuture<T> = future::Ready<Result<T>>;

pub trait ToSocketAddrs {
    type Iter: Iterator<Item = SocketAddr> + Send + 'static;
    type Future: Future<Output = Result<Self::Iter>> + Send + 'static;

    fn to_socket_addrs(&self) -> Self::Future;
}

impl ToSocketAddrs for SocketAddr {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        let iter = Some(*self).into_iter();
        future::ready(Ok(iter))
    }
}

impl ToSocketAddrs for SocketAddrV4 {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        SocketAddr::V4(*self).to_socket_addrs()
    }
}

impl ToSocketAddrs for SocketAddrV6 {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        SocketAddr::V6(*self).to_socket_addrs()
    }
}

impl ToSocketAddrs for (IpAddr, u16) {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        let iter = Some(SocketAddr::from(*self)).into_iter();
        future::ready(Ok(iter))
    }
}

impl ToSocketAddrs for (Ipv4Addr, u16) {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        let (ip, port) = *self;
        SocketAddrV4::new(ip, port).to_socket_addrs()
    }
}

impl ToSocketAddrs for (Ipv6Addr, u16) {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::option::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        let (ip, port) = *self;
        SocketAddrV6::new(ip, port, 0, 0).to_socket_addrs()
    }
}

impl ToSocketAddrs for &[SocketAddr] {
    type Future = ReadyFuture<Self::Iter>;
    type Iter = std::vec::IntoIter<SocketAddr>;

    fn to_socket_addrs(&self) -> Self::Future {
        #[inline]
        fn slice_to_vec(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
            addrs.to_vec()
        }

        // This uses a helper method because clippy doesn't like the `to_vec()`
        // call here (it will allocate, whereas `self.iter().copied()` would
        // not), but it's actually necessary in order to ensure that the
        // returned iterator is valid for the `'static` lifetime, which the
        // borrowed `slice::Iter` iterator would not be.
        let iter = slice_to_vec(self).into_iter();
        future::ready(Ok(iter))
    }
}

#[derive(Debug)]
pub enum OneOrMore {
    One(std::option::IntoIter<SocketAddr>),
    More(std::vec::IntoIter<SocketAddr>),
}

#[derive(Debug)]
enum State {
    Ready(Option<SocketAddr>),
    Blocking(JoinHandle<Result<std::vec::IntoIter<SocketAddr>>>),
}

/// copy from tokio::net::addr
#[derive(Debug)]
pub struct MaybeReady(State);

impl Future for MaybeReady {
    type Output = Result<OneOrMore>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.0 {
            State::Ready(ref mut i) => {
                let iter = OneOrMore::One(i.take().into_iter());
                Poll::Ready(Ok(iter))
            }
            State::Blocking(ref mut rx) => {
                let res = ready!(Pin::new(rx).poll(cx))?.map(OneOrMore::More);

                Poll::Ready(res)
            }
        }
    }
}

impl Iterator for OneOrMore {
    type Item = SocketAddr;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            OneOrMore::One(i) => i.next(),
            OneOrMore::More(i) => i.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            OneOrMore::One(i) => i.size_hint(),
            OneOrMore::More(i) => i.size_hint(),
        }
    }
}

impl ToSocketAddrs for str {
    type Future = MaybeReady;
    type Iter = OneOrMore;

    fn to_socket_addrs(&self) -> Self::Future {
        // First check if the input parses as a socket address
        let res: Result<SocketAddr, _> = self.parse();
        if let Ok(addr) = res {
            return MaybeReady(State::Ready(Some(addr)));
        }

        // Run DNS lookup on the blocking pool
        let s = self.to_owned();

        MaybeReady(State::Blocking(tokio::task::spawn_blocking(move || {
            // Customized dns resolvers are preferred, if a custom resolver does not exist then the
            // standard library's
            get_socket_addrs(&s).map(|v| v.into_iter())
        })))
    }
}

/// Implement this trait for &T of type !Sized(such as str), since &T of type Sized all implement it
/// by default.
impl<T> ToSocketAddrs for &T
where
    T: ToSocketAddrs + ?Sized,
{
    type Future = T::Future;
    type Iter = T::Iter;

    fn to_socket_addrs(&self) -> Self::Future {
        (**self).to_socket_addrs()
    }
}

impl ToSocketAddrs for (&str, u16) {
    type Future = MaybeReady;
    type Iter = OneOrMore;

    fn to_socket_addrs(&self) -> Self::Future {
        let (host, port) = *self;

        // try to parse the host as a regular IP address first
        if let Ok(addr) = host.parse::<Ipv4Addr>() {
            let addr = SocketAddrV4::new(addr, port);
            let addr = SocketAddr::V4(addr);

            return MaybeReady(State::Ready(Some(addr)));
        }

        if let Ok(addr) = host.parse::<Ipv6Addr>() {
            let addr = SocketAddrV6::new(addr, port, 0, 0);
            let addr = SocketAddr::V6(addr);

            return MaybeReady(State::Ready(Some(addr)));
        }

        let host = host.to_owned();

        MaybeReady(State::Blocking(tokio::task::spawn_blocking(move || {
            get_socket_addrs_from_host_port(&host, port).map(|v| v.into_iter())
        })))
    }
}

impl ToSocketAddrs for (String, u16) {
    type Future = MaybeReady;
    type Iter = OneOrMore;

    fn to_socket_addrs(&self) -> Self::Future {
        (self.0.as_str(), self.1).to_socket_addrs()
    }
}

// ===== impl String =====

impl ToSocketAddrs for String {
    type Future = <str as ToSocketAddrs>::Future;
    type Iter = <str as ToSocketAddrs>::Iter;

    fn to_socket_addrs(&self) -> Self::Future {
        self[..].to_socket_addrs()
    }
}

/// Customized dns resolution server
const DEFAULT_DNS_SERVER_GROUP: &[IpAddr] = &[
    IpAddr::V4(Ipv4Addr::new(223, 5, 5, 5)), // alibaba
    IpAddr::V4(Ipv4Addr::new(223, 6, 6, 6)),
    IpAddr::V4(Ipv4Addr::new(119, 29, 29, 29)), // tencent
    IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),      // google
    IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888)), // google
];

const DNS_QUERY_PORT: u16 = 53;

#[inline]
fn custom_resolver_config() -> ResolverConfig {
    ResolverConfig::from_parts(
        None,
        vec![],
        NameServerConfigGroup::from_ips_clear(DEFAULT_DNS_SERVER_GROUP, DNS_QUERY_PORT, true),
    )
}

#[inline]
pub fn get_custom_resolver() -> Option<Resolver> {
    // The sync resolver uses `block_on` internally and will panic if called from a Tokio runtime
    // thread. Keep a guard here so callers can fall back to system DNS, and use the async helpers
    // below when running inside async code.
    if tokio::runtime::Handle::try_current().is_ok() {
        tracing::debug!("Skipping sync custom DNS resolver inside Tokio runtime thread");
        return None;
    }

    match Resolver::new(custom_resolver_config(), ResolverOpts::default()) {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::error!(
                "Create custom dns resolver error:{e},we will use default dns resolver"
            );
            None
        }
    }
}

#[inline]
fn get_custom_async_resolver() -> TokioAsyncResolver {
    static RESOLVER: LazyLock<TokioAsyncResolver> = LazyLock::new(|| {
        TokioAsyncResolver::tokio(custom_resolver_config(), ResolverOpts::default())
    });
    RESOLVER.clone()
}

macro_rules! invalid_input {
    ($msg:expr) => {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, $msg)
    };
}

macro_rules! try_opt {
    ($call:expr, $msg:expr) => {
        match $call {
            Some(v) => v,
            None => Err(invalid_input!($msg))?,
        }
    };
}

fn get_ip_addrs(s: &str) -> Result<Vec<IpAddr>> {
    thread_local! {
        static RESOLVER:Option<Resolver> = get_custom_resolver();
    }
    let result = RESOLVER.with(|r| r.as_ref().map(|r| r.lookup_ip(s)));
    try_opt!(result, "custom resolver not exist")
        .map(|v| v.into_iter().collect())
        .map_err(|e| invalid_input!(e))
}

/// Blocking DNS lookup. Avoid calling this from inside a Tokio runtime thread.
#[inline]
pub fn get_socket_addrs_from_host_port(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    match get_ip_addrs(host) {
        Ok(r) => Ok(r.into_iter().map(|ip| SocketAddr::new(ip, port)).collect()),
        // Resolve dns properly with the standard library
        Err(_) => std::net::ToSocketAddrs::to_socket_addrs(&(host, port)).map(|v| v.collect()),
    }
}

/// Blocking DNS lookup. Avoid calling this from inside a Tokio runtime thread.
#[inline]
pub fn get_socket_addrs(s: &str) -> Result<Vec<SocketAddr>> {
    let (host, port_str) = try_opt!(s.rsplit_once(':'), "invalid socket address");
    let port: u16 = try_opt!(port_str.parse().ok(), "invalid port value");
    get_socket_addrs_from_host_port(host, port)
}

/// Async DNS lookup using the custom resolver, safe to call inside Tokio runtimes.
pub async fn get_ip_addrs_async(s: &str) -> Result<Vec<IpAddr>> {
    let resolver = get_custom_async_resolver();
    resolver
        .lookup_ip(s)
        .await
        .map(|v| v.iter().collect())
        .map_err(|e| invalid_input!(e))
}

/// Async DNS lookup for host + port, falls back to system resolver on failure.
#[inline]
pub async fn get_socket_addrs_from_host_port_async(
    host: &str,
    port: u16,
) -> Result<Vec<SocketAddr>> {
    match get_ip_addrs_async(host).await {
        Ok(r) => Ok(r.into_iter().map(|ip| SocketAddr::new(ip, port)).collect()),
        // Resolve dns properly with the system resolver
        Err(_) => tokio::net::lookup_host((host, port))
            .await
            .map(|v| v.collect())
            .map_err(|e| invalid_input!(e)),
    }
}

/// Async DNS lookup for `domain:port` forms, such as `bilibili.com:1080`.
#[inline]
pub async fn get_socket_addrs_async(s: &str) -> Result<Vec<SocketAddr>> {
    let (host, port_str) = try_opt!(s.rsplit_once(':'), "invalid socket address");
    let port: u16 = try_opt!(port_str.parse().ok(), "invalid port value");
    get_socket_addrs_from_host_port_async(host, port).await
}

pub async fn each_addr<A: ToSocketAddrs, F, T, R>(addr: A, f: F) -> Result<T>
where
    F: Fn(SocketAddr) -> R,
    R: std::future::Future<Output = Result<T>>,
{
    let addrs = match addr.to_socket_addrs().await {
        Ok(addrs) => addrs,
        Err(e) => return Err(e),
    };
    let mut last_err = None;
    for addr in addrs {
        match f(addr).await {
            Ok(l) => return Ok(l),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "could not resolve to any addresses",
        )
    }))
}
