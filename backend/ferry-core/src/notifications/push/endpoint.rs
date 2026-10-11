//! The SSRF guard: which push endpoints the server POSTs to. A subscription's endpoint is a URL the
//! client chose, so it is checked when the subscription is stored and again before every send.

use std::net::IpAddr;

use url::{Host, Origin, Url};

/// Push service hosts, matched exactly: FCM (Chrome and the other Chromium browsers) and Apple
/// (Safari, and web apps on iOS).
const EXACT_HOSTS: [&str; 2] = ["fcm.googleapis.com", "web.push.apple.com"];
/// Push service domains, matched as `<name>.<suffix>`: Mozilla's autopush (Firefox) and Windows
/// Push Notification Services (Edge).
const HOST_SUFFIXES: [&str; 2] = [".push.services.mozilla.com", ".notify.windows.com"];
/// The longest endpoint accepted. Real ones are a few hundred bytes.
pub const MAX_ENDPOINT_LEN: usize = 2048;

/// `endpoint` as a URL when the server may send to it: https on the default port, no user name or
/// password, and a push service's host (above). `stub` is the one other origin allowed, the test
/// stub of `PushSettings` (never set in production).
pub fn check(endpoint: &str, stub: Option<&Origin>) -> Result<Url, &'static str> {
    if endpoint.len() > MAX_ENDPOINT_LEN {
        return Err("is too long");
    }
    let url = Url::parse(endpoint).map_err(|_| "is not an absolute URL")?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must not carry a user name or password");
    }
    if stub.is_some_and(|stub| url.origin() == *stub) {
        return Ok(url);
    }
    if url.scheme() != "https" {
        return Err("must be an https URL");
    }
    // The url crate drops an explicit :443, so any port left is another one.
    if url.port().is_some() {
        return Err("must use the default https port");
    }
    match url.host() {
        Some(Host::Domain(host)) if is_push_service(host) => Ok(url),
        _ => Err("is not a known push service (FCM, Mozilla, Windows or Apple)"),
    }
}

/// True for the hosts of [`EXACT_HOSTS`] and for one or more labels in front of a
/// [`HOST_SUFFIXES`] entry. `host` is lowercase, as the url crate gives it.
pub fn is_push_service(host: &str) -> bool {
    EXACT_HOSTS.contains(&host)
        || HOST_SUFFIXES.iter().any(|suffix| {
            host.strip_suffix(suffix)
                .is_some_and(|name| !name.is_empty() && !name.ends_with('.'))
        })
}

/// `value` as an origin on this machine (`http` or `https`, host 127.0.0.1, [::1] or localhost, no
/// path), for `PUSH_TEST_STUB_ORIGIN`. None otherwise.
pub fn loopback_origin(value: &str) -> Option<Origin> {
    let url = Url::parse(value).ok()?;
    let loopback = match url.host()? {
        Host::Domain(host) => host == "localhost",
        Host::Ipv4(ip) => IpAddr::V4(ip).is_loopback(),
        Host::Ipv6(ip) => IpAddr::V6(ip).is_loopback(),
    };
    let bare = url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    (matches!(url.scheme(), "http" | "https") && loopback && bare).then(|| url.origin())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_push_services_are_allowed() {
        for endpoint in [
            "https://fcm.googleapis.com/fcm/send/dpH5lCsTSSM:APA91bH",
            "https://fcm.googleapis.com/wp/dpH5lCsTSSM",
            "https://updates.push.services.mozilla.com/wpush/v2/gAAAAABk",
            "https://wns2-bl2p.notify.windows.com/w/?token=BQYAAAB",
            "https://web.push.apple.com/QGuQyavXutnMH8",
            "https://FCM.googleapis.com:443/fcm/send/x",
        ] {
            assert!(check(endpoint, None).is_ok(), "{endpoint}");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        for endpoint in [
            "http://fcm.googleapis.com/fcm/send/x",
            "https://fcm.googleapis.com:8443/fcm/send/x",
            "https://user:pw@fcm.googleapis.com/fcm/send/x",
            "https://user@fcm.googleapis.com/fcm/send/x",
            "https://fcm.googleapis.com.evil.example/x",
            "https://evilfcm.googleapis.com/x",
            "https://googleapis.com/x",
            "https://push.services.mozilla.com/x",
            "https://.push.services.mozilla.com/x",
            "https://evilpush.services.mozilla.com/x",
            "https://notify.windows.com.evil.example/x",
            "https://web.push.apple.com.evil.example/x",
            "https://example.com/push",
            "https://localhost/push",
            "https://127.0.0.1/push",
            "https://[::1]/push",
            "https://10.0.0.5/push",
            "https://169.254.169.254/latest/meta-data",
            "https://2130706433/push",
            "ftp://fcm.googleapis.com/x",
            "file:///etc/passwd",
            "/relative/path",
            "",
        ] {
            assert!(check(endpoint, None).is_err(), "{endpoint}");
        }
        let long = format!(
            "https://fcm.googleapis.com/{}",
            "a".repeat(MAX_ENDPOINT_LEN)
        );
        assert_eq!(check(&long, None).unwrap_err(), "is too long");
    }

    #[test]
    fn the_stub_origin_is_the_only_exception() {
        let stub = loopback_origin("http://127.0.0.1:8291").unwrap();
        assert!(check("http://127.0.0.1:8291/push/abc", Some(&stub)).is_ok());
        assert!(check("http://127.0.0.1:8292/push/abc", Some(&stub)).is_err());
        assert!(check("http://localhost:8291/push/abc", Some(&stub)).is_err());
        assert!(check("https://127.0.0.1:8291/push/abc", Some(&stub)).is_err());
        assert!(check("http://u:p@127.0.0.1:8291/push/abc", Some(&stub)).is_err());
        assert!(check("http://127.0.0.1:8291/push/abc", None).is_err());
    }

    #[test]
    fn stub_origins_must_be_on_this_machine() {
        for value in [
            "http://127.0.0.1:8291",
            "http://127.0.0.1:8291/",
            "http://localhost:9000",
            "http://[::1]:9000",
            "https://127.0.0.1:9000",
        ] {
            assert!(loopback_origin(value).is_some(), "{value}");
        }
        for value in [
            "http://10.0.0.5:8291",
            "http://example.com",
            "http://127.0.0.1:8291/push",
            "http://127.0.0.1:8291?x=1",
            "http://u@127.0.0.1:8291",
            "ftp://127.0.0.1",
            "127.0.0.1:8291",
            "",
        ] {
            assert!(loopback_origin(value).is_none(), "{value}");
        }
    }
}
