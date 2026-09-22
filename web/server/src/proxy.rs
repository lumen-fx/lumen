//! Which peers may say where a request came from.
//!
//! A reverse proxy in front of the server is where TLS ends and where the
//! visitor's address is known, and it says both in `X-Forwarded-*` headers.
//! Anyone else can send the same headers, so they are believed only from a
//! peer named as a proxy, and dropped from every other request before a
//! handler or an app sees them.

use std::fmt;
use std::net::IpAddr;

use crate::http::header;

/// An address, or a block of them written as `address/prefix`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Whether `address` is inside this block.
    pub fn contains(&self, address: IpAddr) -> bool {
        match (self.network, canonical(address)) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                let mask = mask32(self.prefix);
                u32::from(network) & mask == u32::from(address) & mask
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                let mask = mask128(self.prefix);
                u128::from(network) & mask == u128::from(address) & mask
            }
            _ => false,
        }
    }
}

/// An IPv4 peer reached over an IPv6 socket arrives as `::ffff:a.b.c.d`, and
/// a proxy is named by its IPv4 address.
fn canonical(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

fn mask32(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    }
}

fn mask128(prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - u32::from(prefix))
    }
}

impl std::str::FromStr for Cidr {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (text, None),
        };
        let network: IpAddr = address
            .parse()
            .map_err(|_| format!("`{text}` is not an address or an address/prefix block"))?;
        let network = canonical(network);
        let width = if network.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => width,
            Some(prefix) => prefix
                .parse::<u8>()
                .ok()
                .filter(|prefix| *prefix <= width)
                .ok_or_else(|| format!("`{text}` has a prefix length outside 0..={width}"))?,
        };
        Ok(Self { network, prefix })
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

/// Which peers the forwarding headers are believed from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Trust {
    /// None: the peer is the visitor, and the headers are dropped.
    #[default]
    Nobody,
    /// Every peer, which suits a server only this machine can reach.
    Everybody,
    /// These blocks.
    Only(Vec<Cidr>),
}

impl Trust {
    fn trusts(&self, peer: IpAddr) -> bool {
        match self {
            Trust::Nobody => false,
            Trust::Everybody => true,
            Trust::Only(blocks) => blocks.iter().any(|block| block.contains(peer)),
        }
    }

    /// Where a request came from and whether it arrived over TLS, and the
    /// headers with anything an untrusted peer said about that taken out.
    pub(crate) fn resolve(
        &self,
        peer: Option<IpAddr>,
        headers: &mut Vec<(String, String)>,
    ) -> (Option<IpAddr>, bool) {
        let trusted = peer.is_some_and(|peer| self.trusts(peer));
        if !trusted {
            headers.retain(|(name, _)| !forwarding(name));
            return (peer, false);
        }
        let secure = header(headers, "x-forwarded-proto").is_some_and(|proto| {
            // A chain of proxies appends; the nearest one is last.
            proto
                .rsplit(',')
                .next()
                .is_some_and(|last| last.trim().eq_ignore_ascii_case("https"))
        });
        // The visitor is the nearest address in the chain that is not itself
        // a proxy this server trusts.
        let mut client = peer;
        let chain: Vec<IpAddr> = headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("x-forwarded-for"))
            .flat_map(|(_, value)| value.split(','))
            .filter_map(|hop| hop.trim().parse().ok())
            .collect();
        for hop in chain.into_iter().rev() {
            client = Some(hop);
            if !self.trusts(hop) {
                break;
            }
        }
        (client, secure)
    }
}

/// Whether `name` is one of the headers a proxy says the origin with.
fn forwarding(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("x-forwarded-") || name == "forwarded" || name == "x-real-ip"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("an address")
    }

    #[test]
    fn a_block_holds_the_addresses_its_prefix_covers() {
        let block: Cidr = "10.0.0.0/8".parse().expect("a block");
        assert!(block.contains(ip("10.1.2.3")));
        assert!(!block.contains(ip("11.0.0.1")));
        // An IPv4 peer on an IPv6 socket is still that IPv4 peer.
        assert!(block.contains(ip("::ffff:10.9.9.9")));
        let one: Cidr = "192.168.1.5".parse().expect("one address");
        assert!(one.contains(ip("192.168.1.5")));
        assert!(!one.contains(ip("192.168.1.6")));
        let v6: Cidr = "fd00::/8".parse().expect("a v6 block");
        assert!(v6.contains(ip("fd12::1")));
        let all: Cidr = "0.0.0.0/0".parse().expect("everything");
        assert!(all.contains(ip("8.8.8.8")));
        assert!("10.0.0.0/33".parse::<Cidr>().is_err());
        assert!("nonsense".parse::<Cidr>().is_err());
    }

    fn forwarded(proto: &str, chain: &str) -> Vec<(String, String)> {
        vec![
            ("X-Forwarded-Proto".to_string(), proto.to_string()),
            ("X-Forwarded-For".to_string(), chain.to_string()),
            ("Accept".to_string(), "*/*".to_string()),
        ]
    }

    #[test]
    fn an_untrusted_peer_is_the_visitor_and_its_forwarding_headers_go() {
        let mut headers = forwarded("https", "1.2.3.4");
        let (client, secure) = Trust::Nobody.resolve(Some(ip("9.9.9.9")), &mut headers);
        assert_eq!(client, Some(ip("9.9.9.9")));
        assert!(!secure);
        assert_eq!(headers.len(), 1, "{headers:?}");
    }

    #[test]
    fn a_trusted_proxy_names_the_visitor_and_the_scheme() {
        let trust = Trust::Only(vec!["10.0.0.0/8".parse().expect("a block")]);
        let mut headers = forwarded("http, https", "6.6.6.6, 1.2.3.4, 10.0.0.7");
        let (client, secure) = trust.resolve(Some(ip("10.0.0.1")), &mut headers);
        // The last hop is a trusted proxy too, so the visitor is the one
        // before it; what the visitor claimed before that is not believed.
        assert_eq!(client, Some(ip("1.2.3.4")));
        assert!(secure);
        assert_eq!(headers.len(), 3);
    }
}
