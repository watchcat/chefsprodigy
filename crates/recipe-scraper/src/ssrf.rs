//! SSRF guard for user-supplied URLs.
//!
//! Everything the importer fetches on a user's behalf (recipe pages, cover
//! images, JSON-LD image URLs) passes [`UrlGuard`]: the scheme must be `http` or
//! `https`, literal IP hosts are checked directly and hostnames are resolved with
//! every answer checked. The same check runs inside the HTTP client, through
//! [`GuardedResolver`] and [`redirect_policy`], so each redirect hop is checked
//! again and the connection goes to the address that was checked rather than to
//! a second lookup that DNS rebinding could swap.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect;
use url::{Host, Url};

use crate::ScraperError;

/// Redirect hops followed before giving up (reqwest's own default).
const MAX_REDIRECTS: usize = 10;

/// Why an address may not be fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedKind {
    Unspecified,
    Loopback,
    /// RFC 1918 (`10/8`, `172.16/12`, `192.168/16`)
    Private,
    /// Carrier-grade NAT (`100.64/10`)
    CarrierGradeNat,
    /// `169.254/16` and `fe80::/10`, where cloud metadata services live
    LinkLocal,
    /// IPv6 unique-local (`fc00::/7`)
    UniqueLocal,
    Multicast,
    /// Reserved, documentation, benchmarking and broadcast ranges
    Reserved,
}

impl BlockedKind {
    /// Ranges a test environment may legitimately run servers on; see
    /// [`UrlGuard::allowing_private_hosts`]. Link-local (cloud metadata),
    /// multicast, unspecified and reserved addresses are never unlocked.
    pub fn unlockable(self) -> bool {
        matches!(
            self,
            Self::Loopback | Self::Private | Self::CarrierGradeNat | Self::UniqueLocal
        )
    }
}

impl fmt::Display for BlockedKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unspecified => "an unspecified address",
            Self::Loopback => "a loopback address",
            Self::Private => "a private address (RFC 1918)",
            Self::CarrierGradeNat => "a carrier-grade NAT address (100.64/10)",
            Self::LinkLocal => "a link-local address (cloud metadata range)",
            Self::UniqueLocal => "a unique-local IPv6 address (fc00::/7)",
            Self::Multicast => "a multicast address",
            Self::Reserved => "a reserved address",
        })
    }
}

/// A host the guard refused, with a sentence explaining why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedHost {
    /// Host as written in the URL
    pub host: String,
    /// Full explanation, e.g. `localhost resolves to 127.0.0.1, a loopback address`
    pub reason: String,
}

impl fmt::Display for BlockedHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for BlockedHost {}

/// Why `ip` must not be fetched, or `None` for a public address. IPv4 addresses
/// embedded in IPv6 (mapped, compatible, NAT64, 6to4) are judged as IPv4.
pub fn blocked_reason(ip: IpAddr) -> Option<BlockedKind> {
    match ip {
        IpAddr::V4(v4) => blocked_v4(v4),
        IpAddr::V6(v6) => blocked_v6(v6),
    }
}

fn blocked_v4(ip: Ipv4Addr) -> Option<BlockedKind> {
    let [a, b, c, _] = ip.octets();
    Some(match (a, b, c) {
        (0, _, _) => BlockedKind::Unspecified,
        (10, _, _) | (172, 16..=31, _) | (192, 168, _) => BlockedKind::Private,
        (100, 64..=127, _) => BlockedKind::CarrierGradeNat,
        (127, _, _) => BlockedKind::Loopback,
        (169, 254, _) => BlockedKind::LinkLocal,
        // 192.0.0.0/24 (IETF protocol assignments, incl. some metadata services),
        // documentation nets, benchmarking, class E and broadcast
        (192, 0, 0) | (192, 0, 2) | (198, 51, 100) | (203, 0, 113) | (198, 18..=19, _) => {
            BlockedKind::Reserved
        }
        (224..=239, _, _) => BlockedKind::Multicast,
        (240.., _, _) => BlockedKind::Reserved,
        _ => return None,
    })
}

fn blocked_v6(ip: Ipv6Addr) -> Option<BlockedKind> {
    if let Some(v4) = embedded_v4(ip) {
        return blocked_v4(v4);
    }
    let seg = ip.segments();
    Some(match seg[0] {
        0 if ip == Ipv6Addr::UNSPECIFIED => BlockedKind::Unspecified,
        0 if ip == Ipv6Addr::LOCALHOST => BlockedKind::Loopback,
        0x0100 if seg[1..4] == [0, 0, 0] => BlockedKind::Reserved, // 100::/64 discard
        0x2001 if seg[1] == 0x0db8 => BlockedKind::Reserved,       // documentation
        s if s & 0xffc0 == 0xfe80 => BlockedKind::LinkLocal,
        s if s & 0xfe00 == 0xfc00 => BlockedKind::UniqueLocal,
        s if s & 0xff00 == 0xff00 => BlockedKind::Multicast,
        _ => return None,
    })
}

/// IPv4 address carried inside a mapped (`::ffff:a.b.c.d`), IPv4-compatible
/// (`::a.b.c.d`), NAT64 (`64:ff9b::a.b.c.d`) or 6to4 (`2002:abcd:efgh::`) address.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    let seg = ip.segments();
    let pair = |i: usize| Ipv4Addr::from((u32::from(seg[i]) << 16) | u32::from(seg[i + 1]));
    match seg {
        [0, 0, 0, 0, 0, 0, _, _] if ip != Ipv6Addr::UNSPECIFIED && ip != Ipv6Addr::LOCALHOST => {
            Some(pair(6))
        }
        [0x64, 0xff9b, 0, 0, 0, 0, _, _] => Some(pair(6)),
        [0x2002, ..] => Some(pair(1)),
        _ => None,
    }
}

/// Check every address a hostname resolved to; one blocked answer refuses the host.
pub fn check_resolved(
    host: &str,
    addrs: impl IntoIterator<Item = IpAddr>,
    allow_private: bool,
) -> Result<(), BlockedHost> {
    for ip in addrs {
        if let Some(kind) = blocked_reason(ip)
            && !(allow_private && kind.unlockable())
        {
            return Err(BlockedHost {
                host: host.to_string(),
                reason: format!("{host} resolves to {ip}, {kind}"),
            });
        }
    }
    Ok(())
}

/// Policy applied to every user-supplied URL before and while fetching it.
#[derive(Debug, Clone, Copy, Default)]
pub struct UrlGuard {
    allow_private: bool,
}

impl UrlGuard {
    /// The production guard: nothing non-public is reachable.
    pub const fn new() -> Self {
        Self {
            allow_private: false,
        }
    }

    /// A guard for tests that run servers on loopback or private networks. It
    /// still refuses link-local (cloud metadata), multicast, unspecified and
    /// reserved addresses and non-http(s) schemes.
    pub const fn allowing_private_hosts() -> Self {
        Self {
            allow_private: true,
        }
    }

    pub fn allows_private_hosts(&self) -> bool {
        self.allow_private
    }

    /// Parse `url` and check it without touching the network: scheme, presence
    /// of a host and literal IP hosts.
    pub fn check_url(&self, url: &str) -> Result<Url, ScraperError> {
        let parsed = Url::parse(url.trim()).map_err(|_| blocked(url, "not an absolute URL"))?;
        self.check_parsed(&parsed)
            .map_err(|b| blocked(url, &b.reason))?;
        Ok(parsed)
    }

    /// `check_url` plus, for hostnames, a lookup with every answer checked. A
    /// failed lookup is not an error here: the request itself reports it.
    pub async fn check(&self, url: &str) -> Result<Url, ScraperError> {
        let parsed = self.check_url(url)?;
        if let Some(Host::Domain(name)) = parsed.host() {
            let port = parsed.port_or_known_default().unwrap_or(80);
            if let Ok(addrs) = tokio::net::lookup_host((name, port)).await {
                check_resolved(name, addrs.map(|a| a.ip()), self.allow_private)
                    .map_err(|b| blocked(url, &b.reason))?;
            }
        }
        Ok(parsed)
    }

    fn check_parsed(&self, url: &Url) -> Result<(), BlockedHost> {
        let host = url.host_str().unwrap_or_default().to_string();
        if !matches!(url.scheme(), "http" | "https") {
            return Err(BlockedHost {
                host,
                reason: format!(
                    "only http and https URLs can be fetched (got {})",
                    url.scheme()
                ),
            });
        }
        match url.host() {
            Some(Host::Ipv4(ip)) => self.check_ip(&host, IpAddr::V4(ip)),
            Some(Host::Ipv6(ip)) => self.check_ip(&host, IpAddr::V6(ip)),
            Some(Host::Domain(_)) => Ok(()),
            None => Err(BlockedHost {
                host,
                reason: "the URL has no host".to_string(),
            }),
        }
    }

    fn check_ip(&self, host: &str, ip: IpAddr) -> Result<(), BlockedHost> {
        match blocked_reason(ip) {
            Some(kind) if !(self.allow_private && kind.unlockable()) => Err(BlockedHost {
                host: host.to_string(),
                reason: format!("{ip} is {kind}"),
            }),
            _ => Ok(()),
        }
    }
}

fn blocked(url: &str, reason: &str) -> ScraperError {
    ScraperError::BlockedUrl {
        url: url.trim().to_string(),
        reason: reason.to_string(),
    }
}

/// DNS resolver for the importer's HTTP client: the system resolver, with every
/// answer checked, so connections (redirect hops included) only ever go to
/// addresses the guard accepted.
#[derive(Debug, Clone)]
pub struct GuardedResolver {
    guard: UrlGuard,
}

impl GuardedResolver {
    pub fn new(guard: UrlGuard) -> Self {
        Self { guard }
    }
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow = self.guard.allow_private;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            check_resolved(&host, addrs.iter().map(|a| a.ip()), allow)?;
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// Redirect policy that re-checks scheme and literal IP hosts on every hop
/// (hostnames are checked by [`GuardedResolver`] when the hop connects).
pub fn redirect_policy(guard: UrlGuard) -> redirect::Policy {
    redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error(std::io::Error::other("too many redirects"));
        }
        match guard.check_parsed(attempt.url()) {
            Ok(()) => attempt.follow(),
            Err(blocked) => attempt.error(blocked),
        }
    })
}

/// Refuse a response that is still a redirect: the client follows only
/// `http`/`https` targets, so a 3xx coming back means the `Location` pointed
/// elsewhere (`file:`, `ftp:`, …) or was missing.
pub fn check_response(url: &str, response: &reqwest::Response) -> Result<(), ScraperError> {
    if !response.status().is_redirection() {
        return Ok(());
    }
    let reason = match response
        .headers()
        .get(reqwest::header::LOCATION)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
    {
        Some(location) => format!(
            "redirect to {location} was not followed: only http and https URLs can be fetched"
        ),
        None => format!(
            "the server answered {} without a Location header",
            response.status()
        ),
    };
    Err(blocked(url, &reason))
}

/// Turn a request failure into a [`ScraperError`], surfacing a guard refusal
/// (from the resolver or the redirect policy) as `BlockedUrl` with its reason.
pub fn fetch_error(url: &str, err: reqwest::Error) -> ScraperError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&err);
    while let Some(e) = source {
        if let Some(b) = e.downcast_ref::<BlockedHost>() {
            return blocked(url, &b.reason);
        }
        source = e.source();
    }
    ScraperError::Http(err)
}
