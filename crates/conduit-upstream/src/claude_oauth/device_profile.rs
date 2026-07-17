//! Claude device fingerprint profile — CLIProxyAPI `claude_device_profile.go` parity.
//!
//! Modes:
//! - **legacy** (default, stabilize=false): OS/Arch = host runtime; UA = client
//!   `claude-cli/*` if present else baseline; package/runtime from client or baseline.
//! - **stabilize** (true): pin OS/Arch to baseline; cache per access-token and
//!   upgrade only when a newer official `claude-cli` UA arrives.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use regex::Regex;
use sha2::{Digest, Sha256};

/// Baseline fingerprint (CLIProxyAPI `defaultClaudeFingerprint*`, 2026-02-28).
pub const DEFAULT_USER_AGENT: &str = "claude-cli/2.1.63 (external, cli)";
pub const DEFAULT_PACKAGE_VERSION: &str = "0.74.0";
pub const DEFAULT_RUNTIME_VERSION: &str = "v24.3.0";
pub const DEFAULT_OS: &str = "MacOS";
pub const DEFAULT_ARCH: &str = "arm64";
pub const DEFAULT_TIMEOUT: &str = "600";

const PROFILE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Debug, Clone, Default)]
pub struct ClaudeHeaderDefaults {
    pub user_agent: Option<String>,
    pub package_version: Option<String>,
    pub runtime_version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub timeout: Option<String>,
    /// When true: stabilize UA/package/runtime per token (CLIProxyAPI).
    pub stabilize_device_profile: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeDeviceProfile {
    pub user_agent: String,
    pub package_version: String,
    pub runtime_version: String,
    pub os: String,
    pub arch: String,
    version: Option<(u32, u32, u32)>,
}

struct CacheEntry {
    profile: ClaudeDeviceProfile,
    expire: Instant,
}

fn profile_cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn claude_cli_version_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^claude-cli/(\d+)\.(\d+)\.(\d+)").expect("ua regex"))
}

pub fn is_claude_code_client(user_agent: &str) -> bool {
    user_agent.trim().starts_with("claude-cli")
}

pub fn parse_claude_cli_version(user_agent: &str) -> Option<(u32, u32, u32)> {
    let caps = claude_cli_version_re().captures(user_agent.trim())?;
    let major = caps.get(1)?.as_str().parse().ok()?;
    let minor = caps.get(2)?.as_str().parse().ok()?;
    let patch = caps.get(3)?.as_str().parse().ok()?;
    Some((major, minor, patch))
}

/// `claude-cli/x.y.z (external, cli)` → `cli`; `(external, vscode)` → `vscode`.
pub fn parse_entrypoint_from_ua(user_agent: &str) -> String {
    let ua = user_agent.trim();
    if let Some(start) = ua.find('(') {
        if let Some(end) = ua[start..].find(')') {
            let inner = &ua[start + 1..start + end];
            // "external, cli" or "external, vscode"
            if let Some((_, ep)) = inner.rsplit_once(',') {
                let ep = ep.trim();
                if !ep.is_empty() {
                    return ep.to_string();
                }
            }
        }
    }
    "cli".into()
}

fn hdr_default(cfg: Option<&str>, fallback: &str) -> String {
    match cfg.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => fallback.to_string(),
    }
}

pub fn map_stainless_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "MacOS",
        "windows" => "Windows",
        "linux" => "Linux",
        "freebsd" => "FreeBSD",
        other => other,
    }
}

pub fn map_stainless_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        other => other,
    }
}

pub fn default_claude_device_profile(defaults: &ClaudeHeaderDefaults) -> ClaudeDeviceProfile {
    let user_agent = hdr_default(defaults.user_agent.as_deref(), DEFAULT_USER_AGENT);
    let version = parse_claude_cli_version(&user_agent);
    ClaudeDeviceProfile {
        user_agent,
        package_version: hdr_default(defaults.package_version.as_deref(), DEFAULT_PACKAGE_VERSION),
        runtime_version: hdr_default(defaults.runtime_version.as_deref(), DEFAULT_RUNTIME_VERSION),
        os: hdr_default(defaults.os.as_deref(), DEFAULT_OS),
        arch: hdr_default(defaults.arch.as_deref(), DEFAULT_ARCH),
        version,
    }
}

fn version_cmp(a: (u32, u32, u32), b: (u32, u32, u32)) -> std::cmp::Ordering {
    a.cmp(&b)
}

fn should_upgrade(candidate: &ClaudeDeviceProfile, current: &ClaudeDeviceProfile) -> bool {
    match (candidate.version, current.version) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(c), Some(cur)) => version_cmp(c, cur) == std::cmp::Ordering::Greater,
    }
}

fn pin_platform(
    mut profile: ClaudeDeviceProfile,
    baseline: &ClaudeDeviceProfile,
) -> ClaudeDeviceProfile {
    profile.os = baseline.os.clone();
    profile.arch = baseline.arch.clone();
    profile
}

fn normalize(
    mut profile: ClaudeDeviceProfile,
    baseline: &ClaudeDeviceProfile,
) -> ClaudeDeviceProfile {
    profile = pin_platform(profile, baseline);
    if profile.user_agent.is_empty()
        || profile.version.is_none()
        || should_upgrade(baseline, &profile)
    {
        profile.user_agent = baseline.user_agent.clone();
        profile.package_version = baseline.package_version.clone();
        profile.runtime_version = baseline.runtime_version.clone();
        profile.version = baseline.version;
    }
    profile
}

fn client_header<'a>(client: &'a [(String, String)], name: &str) -> Option<&'a str> {
    client
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn extract_candidate(
    client: &[(String, String)],
    baseline: &ClaudeDeviceProfile,
) -> Option<ClaudeDeviceProfile> {
    let ua = client_header(client, "User-Agent")?;
    let version = parse_claude_cli_version(ua)?;
    Some(ClaudeDeviceProfile {
        user_agent: ua.to_string(),
        package_version: client_header(client, "X-Stainless-Package-Version")
            .unwrap_or(&baseline.package_version)
            .to_string(),
        runtime_version: client_header(client, "X-Stainless-Runtime-Version")
            .unwrap_or(&baseline.runtime_version)
            .to_string(),
        os: client_header(client, "X-Stainless-Os")
            .unwrap_or(&baseline.os)
            .to_string(),
        arch: client_header(client, "X-Stainless-Arch")
            .unwrap_or(&baseline.arch)
            .to_string(),
        version: Some(version),
    })
}

fn cache_key(access_token: &str) -> String {
    let sum = Sha256::digest(access_token.as_bytes());
    hex::encode(sum)
}

/// Stabilized profile: cache per access token, upgrade on newer official UA.
pub fn resolve_stabilized_device_profile(
    access_token: &str,
    client_headers: &[(String, String)],
    defaults: &ClaudeHeaderDefaults,
) -> ClaudeDeviceProfile {
    let baseline = default_claude_device_profile(defaults);
    let mut candidate =
        extract_candidate(client_headers, &baseline).map(|c| pin_platform(c, &baseline));
    // Only accept candidates newer than baseline floor
    if let Some(ref c) = candidate {
        if !should_upgrade(c, &baseline) {
            candidate = None;
        }
    }

    let key = cache_key(access_token);
    let now = Instant::now();
    let mut guard = profile_cache().lock().unwrap_or_else(|e| e.into_inner());

    if let Some(cand) = candidate {
        if let Some(entry) = guard.get_mut(&key) {
            if entry.expire > now && !entry.profile.user_agent.is_empty() {
                entry.profile = normalize(entry.profile.clone(), &baseline);
                if !should_upgrade(&cand, &entry.profile) {
                    entry.expire = now + PROFILE_TTL;
                    return entry.profile.clone();
                }
            }
        }
        guard.insert(
            key,
            CacheEntry {
                profile: cand.clone(),
                expire: now + PROFILE_TTL,
            },
        );
        return cand;
    }

    if let Some(entry) = guard.get_mut(&key) {
        if entry.expire > now && !entry.profile.user_agent.is_empty() {
            entry.profile = normalize(entry.profile.clone(), &baseline);
            entry.expire = now + PROFILE_TTL;
            return entry.profile.clone();
        }
    }

    baseline
}

/// Legacy profile (CLIProxyAPI default when stabilize is off).
pub fn resolve_legacy_device_profile(
    client_headers: &[(String, String)],
    defaults: &ClaudeHeaderDefaults,
) -> ClaudeDeviceProfile {
    let baseline = default_claude_device_profile(defaults);
    let package_version = client_header(client_headers, "X-Stainless-Package-Version")
        .unwrap_or(&baseline.package_version)
        .to_string();
    let runtime_version = client_header(client_headers, "X-Stainless-Runtime-Version")
        .unwrap_or(&baseline.runtime_version)
        .to_string();

    // Legacy: OS/Arch from host runtime (not baseline MacOS/arm64 pin).
    let os = map_stainless_os().to_string();
    let arch = map_stainless_arch().to_string();

    let client_ua = client_header(client_headers, "User-Agent").unwrap_or("");
    let user_agent = if is_claude_code_client(client_ua) {
        client_ua.to_string()
    } else {
        baseline.user_agent.clone()
    };
    let version = parse_claude_cli_version(&user_agent);

    ClaudeDeviceProfile {
        user_agent,
        package_version,
        runtime_version,
        os,
        arch,
        version,
    }
}

pub fn resolve_device_profile(
    access_token: &str,
    client_headers: &[(String, String)],
    defaults: &ClaudeHeaderDefaults,
) -> ClaudeDeviceProfile {
    if defaults.stabilize_device_profile {
        resolve_stabilized_device_profile(access_token, client_headers, defaults)
    } else {
        resolve_legacy_device_profile(client_headers, defaults)
    }
}

/// Version string for billing header (`2.1.63`).
pub fn profile_claude_version(profile: &ClaudeDeviceProfile) -> String {
    if let Some((maj, min, pat)) = profile.version {
        format!("{maj}.{min}.{pat}")
    } else {
        super::cloak::DEFAULT_CLAUDE_VERSION.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_passthrough_claude_cli_ua() {
        let defaults = ClaudeHeaderDefaults::default();
        let client = vec![
            (
                "User-Agent".into(),
                "claude-cli/2.2.0 (external, cli)".into(),
            ),
            ("X-Stainless-Package-Version".into(), "0.80.0".into()),
        ];
        let p = resolve_legacy_device_profile(&client, &defaults);
        assert_eq!(p.user_agent, "claude-cli/2.2.0 (external, cli)");
        assert_eq!(p.package_version, "0.80.0");
        assert_eq!(p.os, map_stainless_os());
    }

    #[test]
    fn legacy_non_cli_uses_baseline_ua() {
        let defaults = ClaudeHeaderDefaults::default();
        let client = vec![("User-Agent".into(), "curl/8.0".into())];
        let p = resolve_legacy_device_profile(&client, &defaults);
        assert_eq!(p.user_agent, DEFAULT_USER_AGENT);
    }

    #[test]
    fn config_override_baseline_ua() {
        let defaults = ClaudeHeaderDefaults {
            user_agent: Some("claude-cli/2.1.70 (external, cli)".into()),
            ..Default::default()
        };
        let p = default_claude_device_profile(&defaults);
        assert_eq!(p.user_agent, "claude-cli/2.1.70 (external, cli)");
        assert_eq!(p.version, Some((2, 1, 70)));
    }

    #[test]
    fn stabilize_upgrades_and_pins_platform() {
        let defaults = ClaudeHeaderDefaults {
            stabilize_device_profile: true,
            ..Default::default()
        };
        let token = "sk-ant-oat-stabilize-test";
        // Clear any prior entry by using unique token
        let client = vec![(
            "User-Agent".into(),
            "claude-cli/2.4.0 (external, cli)".into(),
        )];
        let p = resolve_stabilized_device_profile(token, &client, &defaults);
        assert_eq!(p.user_agent, "claude-cli/2.4.0 (external, cli)");
        // Platform pinned to baseline (MacOS/arm64), not client
        assert_eq!(p.os, DEFAULT_OS);
        assert_eq!(p.arch, DEFAULT_ARCH);

        // Older candidate should not downgrade
        let older = vec![(
            "User-Agent".into(),
            "claude-cli/2.3.0 (external, cli)".into(),
        )];
        let p2 = resolve_stabilized_device_profile(token, &older, &defaults);
        assert_eq!(p2.user_agent, "claude-cli/2.4.0 (external, cli)");
    }

    #[test]
    fn entrypoint_from_ua() {
        assert_eq!(
            parse_entrypoint_from_ua("claude-cli/2.1.63 (external, cli)"),
            "cli"
        );
        assert_eq!(
            parse_entrypoint_from_ua("claude-cli/2.1.63 (external, vscode)"),
            "vscode"
        );
    }
}
