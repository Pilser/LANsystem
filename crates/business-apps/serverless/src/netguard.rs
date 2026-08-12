//! SSRF guard for outbound calls, webhooks and cron actions: only public
//! http(s) URLs are allowed — loopback, private, link-local and metadata
//! ranges are rejected before any request is made.

use std::net::IpAddr;

/// Is this URL safe to call from the server? (scheme, host checks only —
/// hostnames that resolve to private IPs at connect time are the HTTP
/// client's responsibility, same as zw's `netguard`.)
pub fn valid_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else { return false };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return false;
    }
    let Some(host) = parsed.host_str() else { return false };
    !is_private_host(host)
}

fn is_private_host(host: &str) -> bool {
    let lower = host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") {
        return true;
    }
    match lower.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
        }
        Ok(IpAddr::V6(v6)) => v6.is_loopback() || v6.is_unspecified() || {
            let octets = v6.octets();
            // fe80::/10 link-local, fc00::/7 unique-local, ::ffff:0:0/96 v4-mapped
            octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80
                || (octets[0] & 0xfe) == 0xfc
                || octets[..10].iter().all(|b| *b == 0) && octets[10] == 0xff && octets[11] == 0xff
        },
        Err(_) => false, // hostname: DNS resolution decides
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_private() {
        assert!(!valid_url("http://127.0.0.1:4321/mcp"));
        assert!(!valid_url("http://localhost:8080/"));
        assert!(!valid_url("http://10.0.0.5/x"));
        assert!(!valid_url("http://192.168.1.1/x"));
        assert!(!valid_url("http://172.16.0.1/x"));
        assert!(!valid_url("http://169.254.169.254/latest"));
        assert!(!valid_url("http://[::1]:80/"));
        assert!(!valid_url("ftp://example.com"));
        assert!(!valid_url("not a url"));
    }

    #[test]
    fn allows_public() {
        assert!(valid_url("https://example.com/api"));
        assert!(valid_url("http://example.com:8080/p"));
        assert!(valid_url("https://api.github.com/repos"));
    }
}
