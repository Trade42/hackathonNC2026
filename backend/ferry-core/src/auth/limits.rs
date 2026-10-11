//! The numbers behind Telegram sign-in (docs/DECISIONS.md, Contract, auth-telegram) and the
//! client address the per-address limit counts by.

use std::net::IpAddr;

use axum::http::HeaderMap;

/// A challenge expires 10 minutes after it starts.
pub const CHALLENGE_TTL_MINUTES: i32 = 10;
/// The window of the per-address and global limits.
pub const WINDOW_MINUTES: i32 = 10;
/// Starts per client address per window. Starting sends nothing, so the limits only keep a script
/// from filling the table; this one leaves room for a venue's shared address.
pub const PER_IP: i64 = 20;
/// Starts per window across all clients.
pub const GLOBAL: i64 = 200;
/// After a challenge is verified, the status poll hands out its token once, on the first poll within
/// this long; every other poll answers `verified` without one (PR #17 review MINOR 1). A token not
/// collected in time is never handed out.
pub const TOKEN_POLL_SECONDS: i32 = 60;

/// The bucket for a request that names no client address: no proxy header.
pub const DIRECT: &str = "direct";

/// The client address for the per-address limit.
///
/// ferry-core is reached through a proxy (the Cloudflare tunnel, later Fly.io or Railway), and
/// the server is not given the TCP peer, so the address comes from the proxy's headers:
/// `CF-Connecting-IP`, otherwise the last `X-Forwarded-For` entry (the one the nearest proxy
/// appended). A request with neither counts as [`DIRECT`]. A client that reaches the server
/// without a proxy can forge these headers; the global limit still holds then.
/// An IPv6 address counts by its /64, since one host usually holds a whole /64.
pub fn client_ip(headers: &HeaderMap) -> String {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let from_cf = header("cf-connecting-ip").and_then(parse_ip);
    let from_xff = || {
        headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .next_back()
            .and_then(parse_ip)
    };
    match from_cf.or_else(from_xff) {
        Some(ip) => bucket(ip),
        None => DIRECT.to_owned(),
    }
}

fn parse_ip(value: &str) -> Option<IpAddr> {
    value.trim().parse::<IpAddr>().ok()
}

fn bucket(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                let net = std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0);
                format!("{net}/64")
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn cloudflare_header_wins() {
        let h = headers(&[
            ("cf-connecting-ip", "203.0.113.7"),
            ("x-forwarded-for", "198.51.100.1"),
        ]);
        assert_eq!(client_ip(&h), "203.0.113.7");
    }

    #[test]
    fn last_forwarded_for_entry() {
        let h = headers(&[("x-forwarded-for", "10.9.9.9, 198.51.100.1 , 203.0.113.9")]);
        assert_eq!(client_ip(&h), "203.0.113.9");
        let h = headers(&[
            ("x-forwarded-for", "10.9.9.9"),
            ("x-forwarded-for", "203.0.113.10"),
        ]);
        assert_eq!(client_ip(&h), "203.0.113.10");
    }

    #[test]
    fn junk_or_nothing_is_direct() {
        assert_eq!(client_ip(&HeaderMap::new()), DIRECT);
        assert_eq!(
            client_ip(&headers(&[("cf-connecting-ip", "not an ip")])),
            DIRECT
        );
        assert_eq!(
            client_ip(&headers(&[("x-forwarded-for", "1.2.3.4, junk")])),
            DIRECT
        );
    }

    #[test]
    fn ipv6_counts_by_its_64() {
        let a = client_ip(&headers(&[("cf-connecting-ip", "2001:db8:1:2:aaaa::1")]));
        let b = client_ip(&headers(&[("cf-connecting-ip", "2001:db8:1:2:bbbb::9")]));
        assert_eq!(a, "2001:db8:1:2::/64");
        assert_eq!(a, b);
        let mapped = client_ip(&headers(&[("cf-connecting-ip", "::ffff:203.0.113.7")]));
        assert_eq!(mapped, "203.0.113.7");
    }

    #[test]
    fn buckets_fit_the_column() {
        let longest = client_ip(&headers(&[(
            "cf-connecting-ip",
            "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        )]));
        assert!(longest.len() <= 64, "{longest}");
    }
}
