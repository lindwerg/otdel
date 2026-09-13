//! The network guard: which hosts may be contacted, and which addresses they may
//! resolve to.
//!
//! Checking the host against an allowlist before building a request is necessary and
//! not sufficient. `intranet.example.com` can be an allowed publisher's name in DNS and
//! still resolve to `10.0.0.5`; an attacker who controls a page that ends up in a search
//! result can point a name at `169.254.169.254`; and a name that resolves correctly when
//! it is *checked* can resolve differently when it is *connected to* — the classic
//! DNS-rebinding race.
//!
//! [`GuardedResolver`] closes all three. It is installed as reqwest's DNS resolver, so
//! it is the connector's only source of addresses: it resolves the name itself, drops
//! every address that is not globally routable, and fails the request when nothing
//! survives. The connector can therefore only ever open a socket to an address this
//! module approved — there is no second lookup to race against.
//!
//! Address classification is written out by hand because `IpAddr::is_global` is still
//! unstable. The ranges are from RFC 1918, RFC 3927, RFC 5735, RFC 6598, RFC 4193 and
//! RFC 4291; IPv4-mapped and IPv4-compatible IPv6 addresses are classified by the
//! address they embed, which is how `::ffff:127.0.0.1` would otherwise slip through.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use otdel_core::research_config::HostAllowlist;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tracing::warn;

/// Which host names an adapter may contact at all.
#[derive(Debug, Clone)]
pub enum HostPolicy {
    /// Exactly these names. Used for the configured search endpoint, which is one
    /// address the owner chose and is not subject to the document allowlist.
    Exact(Vec<String>),
    /// The owner's declared publisher allowlist. Used for documents.
    Declared(HostAllowlist),
}

impl HostPolicy {
    pub fn permits(&self, host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return false;
        }
        match self {
            Self::Exact(names) => names.iter().any(|name| name == &host),
            Self::Declared(allowlist) => allowlist.allows(&host),
        }
    }

    /// What the interface shows. Never a secret.
    pub fn describe(&self) -> Vec<String> {
        match self {
            Self::Exact(names) => names.clone(),
            Self::Declared(allowlist) => allowlist.entries().to_vec(),
        }
    }
}

/// A DNS resolver that answers only with addresses this system is willing to connect to.
pub struct GuardedResolver {
    policy: HostPolicy,
    /// Loopback is permitted only for a self-hosted search endpoint the owner configured
    /// on `127.0.0.1`. It is never permitted for a document: a search result that
    /// resolves to this machine is exactly the attack this module exists to stop.
    allow_loopback: bool,
}

impl GuardedResolver {
    pub fn new(policy: HostPolicy, allow_loopback: bool) -> Self {
        Self {
            policy,
            allow_loopback,
        }
    }

    pub fn shared(policy: HostPolicy, allow_loopback: bool) -> Arc<Self> {
        Arc::new(Self::new(policy, allow_loopback))
    }
}

/// Would an adapter with this loopback allowance open a socket to `address`?
///
/// A free function rather than a method: the resolver's future cannot borrow `self`, and
/// having one definition that both the future and the tests call is what keeps the rule
/// in a single place.
pub fn accepts_address(address: IpAddr, allow_loopback: bool) -> bool {
    (allow_loopback && address.is_loopback()) || is_globally_routable(address)
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let permitted = self.policy.permits(&host);
        let allow_loopback = self.allow_loopback;

        Box::pin(async move {
            if !permitted {
                // Unreachable through the normal path (the caller checks the host first),
                // so reaching it means a redirect or a bug pointed the client somewhere
                // it was never allowed to go.
                warn!(
                    host = %host,
                    "refused to resolve a host outside the configured policy"
                );
                return Err(Box::<dyn std::error::Error + Send + Sync>::from(
                    "host is not permitted by the research configuration",
                ));
            }

            let resolved = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|error| {
                    Box::<dyn std::error::Error + Send + Sync>::from(format!(
                        "could not resolve the host: {error}"
                    ))
                })?;

            let approved: Vec<SocketAddr> = resolved
                .filter(|address| accepts_address(address.ip(), allow_loopback))
                .collect();

            if approved.is_empty() {
                // The address is deliberately not included in the message: it is the
                // interesting part for an attacker probing an internal range, and the
                // owner only needs to know the host was refused.
                warn!(
                    host = %host,
                    "host resolved only to addresses that are not globally routable; refused"
                );
                return Err(Box::<dyn std::error::Error + Send + Sync>::from(
                    "host resolves to an address that is not globally routable",
                ));
            }

            Ok(Box::new(approved.into_iter()) as Addrs)
        })
    }
}

/// Is this an address on the public internet?
///
/// Conservative by construction: anything not clearly public is refused. A false
/// negative costs one unreadable source; a false positive is a request forgery.
pub fn is_globally_routable(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => is_global_v4(v4),
        IpAddr::V6(v6) => is_global_v6(v6),
    }
}

fn is_global_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    // "This network" (0.0.0.0/8), loopback, private, link-local, multicast, broadcast.
    if a == 0
        || address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_multicast()
        || address.is_broadcast()
        || address.is_unspecified()
    {
        return false;
    }
    // Carrier-grade NAT (100.64.0.0/10, RFC 6598) — routable-looking, not public.
    if a == 100 && (64..=127).contains(&b) {
        return false;
    }
    // IETF protocol assignments (192.0.0.0/24) and TEST-NET-1 (192.0.2.0/24).
    if a == 192 && b == 0 && (c == 0 || c == 2) {
        return false;
    }
    // 6to4 relay anycast (192.88.99.0/24).
    if a == 192 && b == 88 && c == 99 {
        return false;
    }
    // Benchmarking (198.18.0.0/15) and TEST-NET-2 (198.51.100.0/24).
    if a == 198 && (b == 18 || b == 19) {
        return false;
    }
    if a == 198 && b == 51 && c == 100 {
        return false;
    }
    // TEST-NET-3 (203.0.113.0/24).
    if a == 203 && b == 0 && c == 113 {
        return false;
    }
    // Reserved for future use (240.0.0.0/4).
    if a >= 240 {
        return false;
    }
    true
}

fn is_global_v6(address: Ipv6Addr) -> bool {
    if address.is_loopback() || address.is_unspecified() || address.is_multicast() {
        return false;
    }

    // An address that merely carries an IPv4 one is judged as that IPv4 address:
    // `::ffff:169.254.169.254` is the metadata service however it is spelled.
    if let Some(v4) = address.to_ipv4_mapped() {
        return is_global_v4(v4);
    }
    if let Some(v4) = address.to_ipv4() {
        return is_global_v4(v4);
    }

    let segments = address.segments();

    // Two more ways an IPv4 address travels inside an IPv6 one, neither of which
    // `to_ipv4_mapped`/`to_ipv4` extracts. On a network that runs either, they are a way
    // to name an internal host that looks globally routable.
    //
    // NAT64, 64:ff9b::/96 and 64:ff9b:1::/48 (RFC 6052, RFC 8215): the last 32 bits.
    if segments[0] == 0x0064 && segments[1] == 0xff9b {
        let [.., a, b, c, d] = address.octets();
        return is_global_v4(Ipv4Addr::new(a, b, c, d));
    }
    // 6to4, 2002::/16 (RFC 3056): the embedded IPv4 is segments 1 and 2.
    if segments[0] == 0x2002 {
        let octets = address.octets();
        return is_global_v4(Ipv4Addr::new(octets[2], octets[3], octets[4], octets[5]));
    }
    // Unique local addresses, fc00::/7.
    if segments[0] & 0xfe00 == 0xfc00 {
        return false;
    }
    // Link-local unicast, fe80::/10.
    if segments[0] & 0xffc0 == 0xfe80 {
        return false;
    }
    // Documentation, 2001:db8::/32.
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return false;
    }
    // IETF protocol assignments, 2001::/23 (includes Teredo 2001::/32).
    if segments[0] == 0x2001 && segments[1] < 0x0200 {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn ip(value: &str) -> IpAddr {
        IpAddr::from_str(value).expect("test address")
    }

    #[test]
    fn every_internal_range_is_refused() {
        for address in [
            // The metadata services of the three big clouds, and the loopback family.
            "169.254.169.254",
            "127.0.0.1",
            "127.1.2.3",
            "0.0.0.0",
            "10.0.0.5",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.1",
            "100.64.0.1",
            "192.0.0.1",
            "192.0.2.5",
            "192.88.99.1",
            "198.18.0.1",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            // IPv6.
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            // …and the same targets wearing an IPv6 costume: mapped, compatible,
            // NAT64-translated and 6to4-encapsulated.
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:5",     // NAT64 of 10.0.0.5
            "64:ff9b::a9fe:a9fe", // NAT64 of 169.254.169.254
            "2002:a00:5::",       // 6to4 of 10.0.0.5
            "2002:a9fe:a9fe::",   // 6to4 of 169.254.169.254
        ] {
            assert!(
                !is_globally_routable(ip(address)),
                "{address} must never be connected to"
            );
        }
    }

    #[test]
    fn ordinary_public_addresses_are_allowed() {
        for address in [
            "93.184.216.34",
            "8.8.8.8",
            "1.1.1.1",
            "2606:2800:220:1:248:1893:25c8:1946",
            "2a00:1450:4001:80e::200e",
        ] {
            assert!(
                is_globally_routable(ip(address)),
                "{address} is a normal public address"
            );
        }
    }

    #[test]
    fn the_declared_allowlist_decides_which_hosts_may_be_resolved() {
        let allowlist = HostAllowlist::parse("docs.example.org, .gost.ru").unwrap();
        let policy = HostPolicy::Declared(allowlist);

        assert!(policy.permits("docs.example.org"));
        assert!(policy.permits("DOCS.EXAMPLE.ORG."));
        assert!(policy.permits("www.gost.ru"));
        assert!(!policy.permits("evil.example.org"));
        assert!(!policy.permits("example.org"));
        assert!(!policy.permits("localhost"));
        assert!(!policy.permits(""));
    }

    #[test]
    fn the_search_endpoint_policy_names_exactly_one_host() {
        let policy = HostPolicy::Exact(vec!["search.example.com".to_owned()]);
        assert!(policy.permits("search.example.com"));
        assert!(!policy.permits("api.search.example.com"));
        assert!(!policy.permits("search.example.com.evil.net"));
        assert_eq!(policy.describe(), vec!["search.example.com".to_owned()]);
    }

    #[test]
    fn a_document_fetcher_never_accepts_loopback_even_when_a_name_points_at_it() {
        assert!(!accepts_address(ip("127.0.0.1"), false));
        assert!(!accepts_address(ip("::1"), false));
        assert!(accepts_address(ip("93.184.216.34"), false));

        // A self-hosted search engine on this machine is the one configuration where
        // loopback is the intended destination.
        assert!(accepts_address(ip("127.0.0.1"), true));
        // …and even then, a private range is not loopback.
        assert!(!accepts_address(ip("10.0.0.5"), true));
        assert!(!accepts_address(ip("169.254.169.254"), true));
    }

    /// `Addrs` is a boxed iterator and has no `Debug`, so `expect_err` cannot be used.
    async fn refusal_reason(resolver: &GuardedResolver, host: &str) -> String {
        match resolver.resolve(Name::from_str(host).unwrap()).await {
            Ok(_) => panic!("`{host}` must not have resolved"),
            Err(error) => error.to_string(),
        }
    }

    #[tokio::test]
    async fn resolving_a_host_outside_the_policy_fails_without_a_lookup() {
        let resolver = GuardedResolver::new(
            HostPolicy::Declared(HostAllowlist::parse("docs.example.org").unwrap()),
            false,
        );
        let reason = refusal_reason(&resolver, "evil.example.com").await;
        assert!(
            reason.contains("not permitted"),
            "unexpected reason: {reason}"
        );
    }

    #[tokio::test]
    async fn a_permitted_host_that_only_resolves_internally_is_refused() {
        // `localhost` is permitted by this policy and resolves to loopback, which the
        // document resolver refuses — the rebinding case, made concrete without a network.
        let resolver = GuardedResolver::new(HostPolicy::Exact(vec!["localhost".to_owned()]), false);
        let reason = refusal_reason(&resolver, "localhost").await;
        assert!(
            reason.contains("globally routable"),
            "unexpected reason: {reason}"
        );
    }
}
