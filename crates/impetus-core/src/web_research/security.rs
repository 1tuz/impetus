use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use reqwest::Url;
use serde::{Deserialize, Serialize};

use super::{WebError, WebErrorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressClass {
    Public,
    Private,
    NonRoutable,
}

#[derive(Debug, Clone)]
pub struct EgressPolicy {
    pub allow_private_network: bool,
    pub allowed_ports: BTreeSet<u16>,
    pub blocked_host_suffixes: Vec<String>,
}

impl Default for EgressPolicy {
    fn default() -> Self {
        Self {
            allow_private_network: false,
            allowed_ports: BTreeSet::from([80, 443]),
            blocked_host_suffixes: vec![
                "localhost".into(),
                ".localhost".into(),
                ".local".into(),
                ".internal".into(),
                ".home.arpa".into(),
            ],
        }
    }
}

impl EgressPolicy {
    pub fn validate_url(&self, raw: &str) -> Result<Url, WebError> {
        let url = Url::parse(raw).map_err(|error| {
            WebError::new(WebErrorKind::InvalidUrl, format!("invalid URL: {error}")).with_url(raw)
        })?;

        match url.scheme() {
            "http" | "https" => {}
            scheme => {
                return Err(WebError::new(
                    WebErrorKind::UnsupportedScheme,
                    format!("scheme '{scheme}' is not allowed for web research"),
                )
                .with_url(raw));
            }
        }

        if !url.username().is_empty() || url.password().is_some() {
            return Err(WebError::new(
                WebErrorKind::CredentialsInUrl,
                "credentials embedded in URLs are not allowed",
            )
            .with_url(raw));
        }

        let port = url.port_or_known_default().ok_or_else(|| {
            WebError::new(WebErrorKind::PortBlocked, "URL has no usable port").with_url(raw)
        })?;
        if !self.allowed_ports.contains(&port) {
            return Err(WebError::new(
                WebErrorKind::PortBlocked,
                format!("port {port} is not allowed by web egress policy"),
            )
            .with_url(raw));
        }

        let host = url.host_str().ok_or_else(|| {
            WebError::new(WebErrorKind::InvalidUrl, "URL does not contain a host").with_url(raw)
        })?;
        let normalized_host = host.trim_end_matches('.').to_ascii_lowercase();
        if !self.allow_private_network && self.host_is_blocked(&normalized_host) {
            return Err(WebError::new(
                WebErrorKind::HostBlocked,
                format!("host '{host}' is blocked by web egress policy"),
            )
            .with_url(raw));
        }

        if let Ok(address) = normalized_host.parse::<IpAddr>() {
            self.enforce_address(address, raw)?;
        }

        Ok(url)
    }

    pub fn enforce_address(&self, address: IpAddr, url: &str) -> Result<(), WebError> {
        match classify_address(address) {
            AddressClass::Public => Ok(()),
            AddressClass::Private if self.allow_private_network => Ok(()),
            AddressClass::Private => Err(WebError::new(
                WebErrorKind::AddressBlocked,
                format!("private/local address {address} is blocked"),
            )
            .with_url(url)),
            AddressClass::NonRoutable => Err(WebError::new(
                WebErrorKind::AddressBlocked,
                format!("non-routable/special address {address} is blocked"),
            )
            .with_url(url)),
        }
    }

    fn host_is_blocked(&self, host: &str) -> bool {
        self.blocked_host_suffixes.iter().any(|entry| {
            if let Some(suffix) = entry.strip_prefix('.') {
                host == suffix || host.ends_with(entry)
            } else {
                host == entry
            }
        })
    }
}

pub fn classify_address(address: IpAddr) -> AddressClass {
    match address {
        IpAddr::V4(address) => classify_v4(address),
        IpAddr::V6(address) => classify_v6(address),
    }
}

fn classify_v4(address: Ipv4Addr) -> AddressClass {
    let octets = address.octets();
    let first = octets[0];
    let second = octets[1];

    if first == 0
        || address.is_unspecified()
        || address.is_multicast()
        || address == Ipv4Addr::BROADCAST
        || first >= 240
        || (first == 192 && second == 0 && octets[2] == 0)
        || (first == 192 && second == 0 && octets[2] == 2)
        || (first == 192 && second == 88 && octets[2] == 99)
        || (first == 198 && (18..=19).contains(&second))
        || (first == 198 && second == 51 && octets[2] == 100)
        || (first == 203 && second == 0 && octets[2] == 113)
    {
        return AddressClass::NonRoutable;
    }

    if address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || (first == 100 && (64..=127).contains(&second))
    {
        return AddressClass::Private;
    }

    AddressClass::Public
}

fn classify_v6(address: Ipv6Addr) -> AddressClass {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return classify_v4(mapped);
    }

    let segments = address.segments();
    let first = segments[0];

    if address.is_unspecified()
        || address.is_multicast()
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
    {
        return AddressClass::NonRoutable;
    }

    if address.is_loopback()
        || (first & 0xfe00) == 0xfc00 // fc00::/7 unique local
        || (first & 0xffc0) == 0xfe80 // fe80::/10 link local
        || (first & 0xffc0) == 0xfec0
    // fec0::/10 deprecated site local
    {
        return AddressClass::Private;
    }

    AddressClass::Public
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_schemes_and_credentials() {
        let policy = EgressPolicy::default();
        assert_eq!(
            policy.validate_url("file:///etc/passwd").unwrap_err().kind,
            WebErrorKind::UnsupportedScheme
        );
        assert_eq!(
            policy
                .validate_url("https://user:secret@example.com/")
                .unwrap_err()
                .kind,
            WebErrorKind::CredentialsInUrl
        );
    }

    #[test]
    fn rejects_local_hosts_and_unapproved_ports() {
        let policy = EgressPolicy::default();
        assert_eq!(
            policy
                .validate_url("http://localhost:80/")
                .unwrap_err()
                .kind,
            WebErrorKind::HostBlocked
        );
        assert_eq!(
            policy
                .validate_url("https://example.com:8443/")
                .unwrap_err()
                .kind,
            WebErrorKind::PortBlocked
        );
    }

    #[test]
    fn classifies_private_and_special_ranges() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
        ] {
            assert_eq!(
                classify_address(ip.parse().unwrap()),
                AddressClass::Private,
                "{ip}"
            );
        }

        for ip in [
            "0.0.0.0",
            "192.0.2.1",
            "198.51.100.3",
            "203.0.113.9",
            "2001:db8::1",
        ] {
            assert_eq!(
                classify_address(ip.parse().unwrap()),
                AddressClass::NonRoutable,
                "{ip}"
            );
        }

        assert_eq!(
            classify_address("1.1.1.1".parse().unwrap()),
            AddressClass::Public
        );
    }

    #[test]
    fn private_network_requires_explicit_opt_in() {
        let mut policy = EgressPolicy::default();
        assert!(
            policy
                .enforce_address("10.1.2.3".parse().unwrap(), "http://10.1.2.3")
                .is_err()
        );
        policy.allow_private_network = true;
        assert!(
            policy
                .enforce_address("10.1.2.3".parse().unwrap(), "http://10.1.2.3")
                .is_ok()
        );
        assert!(
            policy
                .enforce_address("192.0.2.1".parse().unwrap(), "http://192.0.2.1")
                .is_err()
        );
    }
}
