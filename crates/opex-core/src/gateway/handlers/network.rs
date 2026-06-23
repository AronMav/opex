use axum::{Router, extract::State, Json, routing::get};
use serde_json::json;

use crate::gateway::clusters::StatusMonitor;
use crate::gateway::state::{AppState, WanIpCache};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/network/addresses", get(api_network_addresses))
}

// ── CGNAT / RFC-1918 classification ──────────────────────────────────────────

/// Returns true if the IPv4 address falls in a CGNAT (RFC 6598: 100.64.0.0/10)
/// or RFC 1918 private range.  IPv6 addresses always return false here because
/// Tailscale uses the 100.x space, and we only need to flag NAT situations.
pub(crate) fn is_cgnat_or_private(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            // RFC 6598 CGNAT: 100.64.0.0/10  (100.64.x.x – 100.127.x.x)
            if o[0] == 100 && (o[1] & 0xC0) == 64 {
                return true;
            }
            // RFC 1918: 10.0.0.0/8
            if o[0] == 10 {
                return true;
            }
            // RFC 1918: 172.16.0.0/12
            if o[0] == 172 && (16..=31).contains(&o[1]) {
                return true;
            }
            // RFC 1918: 192.168.0.0/16
            if o[0] == 192 && o[1] == 168 {
                return true;
            }
            false
        }
        std::net::IpAddr::V6(_) => false,
    }
}

// ── Tailscale subprocess ──────────────────────────────────────────────────────

/// Runs `tailscale status --json` with a 3-second timeout.
/// Returns the parsed JSON value, or None if the binary is absent / not running.
async fn detect_tailscale() -> Option<serde_json::Value> {
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::process::Command::new("tailscale")
            .args(["status", "--json"])
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) if output.status.success() => {
            serde_json::from_slice(&output.stdout).ok()
        }
        _ => None,
    }
}

// ── LAN interface enumeration ─────────────────────────────────────────────────

/// Returns all non-loopback interface addresses as JSON objects.
async fn enumerate_lan_addresses() -> Vec<serde_json::Value> {
    let ifaces = tokio::task::spawn_blocking(if_addrs::get_if_addrs)
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .unwrap_or_default();

    ifaces
        .into_iter()
        .filter(|iface| !iface.is_loopback())
        .map(|iface| {
            let ip = iface.ip();
            json!({
                "interface": iface.name,
                "ip": ip.to_string(),
                "is_ipv6": ip.is_ipv6(),
            })
        })
        .collect()
}

// ── WAN IP (cached) ───────────────────────────────────────────────────────────

/// Fetches the public WAN IP, using a 5-minute in-process cache to avoid
/// hammering external lookup services on every /api/doctor call.
async fn fetch_wan_ip(status: &StatusMonitor) -> serde_json::Value {
    const CACHE_TTL_SECS: u64 = 300;

    // Check cache
    {
        let cache = status.wan_ip_cache.read().await;
        if let Some(ref cached) = *cache
            && cached.fetched_at.elapsed().as_secs() < CACHE_TTL_SECS {
                return json!({
                    "ip": cached.ip,
                    "is_cgnat": cached.is_cgnat,
                });
            }
    }

    // Cache miss — perform lookup (bounded to avoid indefinite hang)
    let lookup_result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        public_ip_address::perform_lookup(None),
    )
    .await;
    match lookup_result.ok().and_then(std::result::Result::ok) {
        Some(info) => {
            let ip_addr = info.ip;
            let ip_str = ip_addr.to_string();
            let is_cgnat = is_cgnat_or_private(&ip_addr);

            let entry = WanIpCache {
                ip: ip_str.clone(),
                is_cgnat,
                fetched_at: std::time::Instant::now(),
            };
            *status.wan_ip_cache.write().await = Some(entry);

            json!({
                "ip": ip_str,
                "is_cgnat": is_cgnat,
            })
        }
        None => {
            json!({
                "ip": null,
                "is_cgnat": null,
                "cgnat_warning": "WAN IP detection failed",
            })
        }
    }
}

// ── Full network summary ──────────────────────────────────────────────────────

/// Assembles the full network summary used by both `/api/network/addresses` and
/// `/api/doctor` (as the "network" check details).
pub(crate) async fn fetch_network_summary(status: &StatusMonitor) -> serde_json::Value {
    let (tailscale_raw, lan) =
        tokio::join!(detect_tailscale(), enumerate_lan_addresses());

    let mut wan = fetch_wan_ip(status).await;

    // Build Tailscale block
    let tailscale = match tailscale_raw {
        Some(ref ts) => {
            let backend_state = ts["BackendState"].as_str().unwrap_or("").to_string();
            let connected = backend_state == "Running";

            // Collect Tailscale IPs
            let ts_ips: Vec<String> = ts["TailscaleIPs"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
                        .collect()
                })
                .unwrap_or_default();

            // Strip trailing dot from DNS name
            let dns_name = ts["Self"]["DNSName"]
                .as_str()
                .map(|s| s.trim_end_matches('.').to_string());

            // Cross-check: if WAN IP appears in Tailscale IPs, it's not actually CGNAT
            if let Some(wan_ip_str) = wan["ip"].as_str()
                && ts_ips.iter().any(|tip| tip == wan_ip_str) {
                    wan["is_cgnat"] = json!(false);
                    wan["source"] = json!("tailscale");
                }

            json!({
                "connected": connected,
                "backend_state": backend_state,
                "ips": ts_ips,
                "dns_name": dns_name,
            })
        }
        None => serde_json::Value::Null,
    };

    let mdns = json!({ "hostname": "hydeclaw.local" });

    json!({
        "wan": wan,
        "tailscale": tailscale,
        "lan": lan,
        "mdns": mdns,
    })
}

// ── Route handler ─────────────────────────────────────────────────────────────

/// GET /api/network/addresses
pub(crate) async fn api_network_addresses(
    State(status): State<StatusMonitor>,
) -> Json<serde_json::Value> {
    let summary = fetch_network_summary(&status).await;
    Json(summary)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    #[test]
    fn test_is_cgnat_100_64() {
        let ip: IpAddr = "100.64.0.1".parse().unwrap();
        assert!(is_cgnat_or_private(&ip), "100.64.0.1 is in CGNAT range RFC 6598");
    }

    #[test]
    fn test_is_cgnat_100_127() {
        let ip: IpAddr = "100.127.255.255".parse().unwrap();
        assert!(is_cgnat_or_private(&ip), "100.127.255.255 is in CGNAT range RFC 6598");
    }

    #[test]
    fn test_is_private_10() {
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(is_cgnat_or_private(&ip), "10.0.0.1 is RFC 1918 private");
    }

    #[test]
    fn test_is_private_172() {
        let ip: IpAddr = "172.16.0.1".parse().unwrap();
        assert!(is_cgnat_or_private(&ip), "172.16.0.1 is RFC 1918 private");
    }

    #[test]
    fn test_is_private_192_168() {
        let ip: IpAddr = "192.168.1.1".parse().unwrap();
        assert!(is_cgnat_or_private(&ip), "192.168.1.1 is RFC 1918 private");
    }

    #[test]
    fn test_is_not_cgnat_public() {
        let ip: IpAddr = "8.8.8.8".parse().unwrap();
        assert!(!is_cgnat_or_private(&ip), "8.8.8.8 is a public IP");
    }

    #[test]
    fn test_is_not_cgnat_100_63() {
        // 100.63.x.x is just below the CGNAT range — it's public
        let ip: IpAddr = "100.63.255.255".parse().unwrap();
        assert!(!is_cgnat_or_private(&ip), "100.63.255.255 is below CGNAT range, thus public");
    }

    #[test]
    fn test_is_not_cgnat_100_128() {
        // 100.128.x.x is just above the CGNAT range — it's public
        let ip: IpAddr = "100.128.0.1".parse().unwrap();
        assert!(!is_cgnat_or_private(&ip), "100.128.0.1 is above CGNAT range, thus public");
    }

    #[tokio::test]
    async fn test_tailscale_absent_no_panic() {
        // tailscale binary is unlikely to be present in CI; should return None gracefully
        let result = detect_tailscale().await;
        // Either None (binary absent) or Some (binary present) — the important thing is no panic
        let _ = result; // Just ensure it returns without panic
    }

    #[tokio::test]
    async fn test_lan_addresses_non_empty_no_loopback() {
        let addrs = enumerate_lan_addresses().await;
        // On any real machine there should be at least one non-loopback interface
        assert!(!addrs.is_empty(), "expected at least one non-loopback interface");
        // Loopback addresses must not appear
        for addr in &addrs {
            let ip_str = addr["ip"].as_str().unwrap_or("");
            assert_ne!(ip_str, "127.0.0.1", "loopback 127.0.0.1 must be excluded");
            assert_ne!(ip_str, "::1", "loopback ::1 must be excluded");
        }
    }

    #[tokio::test]
    async fn network_addresses_returns_ok() {
        let status = StatusMonitor::test_new();
        let Json(resp) = api_network_addresses(axum::extract::State(status)).await;
        // Response must be a JSON object with at least the expected top-level keys
        assert!(resp.is_object(), "response must be a JSON object");
        assert!(resp.get("lan").is_some(), "response must contain 'lan' key");
        assert!(resp.get("wan").is_some(), "response must contain 'wan' key");
    }
}
