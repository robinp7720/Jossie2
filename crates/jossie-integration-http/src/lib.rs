use base64::Engine;
use jossie_core::integration::{Integration, ToolDefinition};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use url::Url;

fn is_allowed_test_host(host: &str) -> bool {
    #[cfg(test)]
    {
        host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
    }

    #[cfg(not(test))]
    {
        let _ = host;
        false
    }
}

fn is_public_ipv4(addr: Ipv4Addr) -> bool {
    let octets = addr.octets();
    !addr.is_private()
        && !addr.is_loopback()
        && !addr.is_link_local()
        && !addr.is_broadcast()
        && !addr.is_documentation()
        && !addr.is_unspecified()
        && !addr.is_multicast()
        && octets[0] != 0
        && octets[0] != 127
        && !(octets[0] == 10 && octets[1] == 0) // Already covered by is_private, but being explicit
        && !(octets[0] == 172 && (octets[1] >= 16 && octets[1] <= 31)) // Already covered by is_private
        && !(octets[0] == 192 && octets[1] == 168) // Already covered by is_private
        && !(octets[0] == 100 && (octets[1] & 0b1100_0000) == 0b0100_0000)
        && !(octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        && !(octets[0] >= 224) // Multicast/Reserved
    // Shared address space 100.64.0.0/10 and benchmarking 198.18.0.0/15.
}

fn is_public_ipv6(addr: Ipv6Addr) -> bool {
    let segments = addr.segments();
    !addr.is_loopback()
        && !addr.is_unspecified()
        && !addr.is_multicast()
        && !addr.is_unique_local()
        && !addr.is_unicast_link_local()
        && !(segments[0] == 0x2001 && segments[1] == 0x0db8) // Documentation
        && !(segments[0] & 0xffc0 == 0xfe80) // Link-local (redundant but explicit)
        && !(segments[0] & 0xfe00 == 0xfc00) // Unique local (redundant but explicit)
}

fn is_public_ip(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(addr) => is_public_ipv4(addr),
        IpAddr::V6(addr) => is_public_ipv6(addr),
    }
}

async fn validate_url_target(url: &Url) -> anyhow::Result<()> {
    if url.scheme() != "http" && url.scheme() != "https" {
        anyhow::bail!("Blocked: URL must use http or https.");
    }

    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("Blocked: URL is missing a host."))?;
    if is_allowed_test_host(host) {
        return Ok(());
    }
    if host.eq_ignore_ascii_case("localhost") {
        anyhow::bail!("Blocked: URL targets a local hostname.");
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            anyhow::bail!("Blocked: URL targets a local or private IP address.");
        }
        return Ok(());
    }

    let port = url.port_or_known_default().unwrap_or(80);
    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| anyhow::anyhow!("Blocked: failed to resolve host '{host}': {e}"))?;

    let mut had_addresses = false;
    for socket_addr in resolved {
        had_addresses = true;
        if !is_public_ip(socket_addr.ip()) {
            anyhow::bail!(
                "Blocked: host '{}' resolved to a local or private address ({})",
                host,
                socket_addr.ip()
            );
        }
    }

    if !had_addresses {
        anyhow::bail!("Blocked: host '{host}' did not resolve to any addresses.");
    }

    Ok(())
}

pub struct HttpIntegration {
    allowed_domains: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct MultipartField {
    name: String,
    value: String,
}

#[derive(Deserialize, Debug)]
struct MultipartFile {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    data_base64: String,
}

#[derive(Deserialize, Debug)]
struct MultipartBody {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    body_type: String, // must be "multipart"
    fields: Option<Vec<MultipartField>>,
    files: Option<Vec<MultipartFile>>,
}

enum BodyContent {
    None,
    Text(String),
    Json(Vec<u8>), // already serialized bytes
    Multipart(reqwest::multipart::Form),
}

include!("http/logging.rs");
include!("http/request.rs");
include!("http/integration.rs");
include!("http/tests.rs");
