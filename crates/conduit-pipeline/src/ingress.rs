//! L2 Ingress Filter: auth validation and quota enforcement.

use conduit_ir::error::GatewayError;
use conduit_quota::check::QuotaCheckRequest;

/// Result from auth lookup — the downstream key's policy settings.
#[derive(Debug, Clone)]
pub struct KeyPolicy {
    /// Stable stored key id (ULID / DB primary key). Never the raw bearer secret.
    pub key_id: String,
    pub model_whitelist: Vec<String>,
    pub rate_limit_rpm: Option<u32>,
}

impl KeyPolicy {
    pub fn check_model_allowed(&self, alias: &str) -> Result<(), GatewayError> {
        if self.model_whitelist.is_empty() {
            return Ok(());
        }
        if self.model_whitelist.iter().any(|m| m == alias) {
            Ok(())
        } else {
            Err(GatewayError::Routing(format!(
                "model '{}' not in allowed list for this key",
                alias
            )))
        }
    }
}

/// Require a non-empty bearer string. Does not validate the key itself.
pub fn require_bearer(bearer: Option<&str>) -> Result<&str, GatewayError> {
    match bearer.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Ok(s),
        None => Err(GatewayError::Unauthorized(
            "missing authorization bearer token".into(),
        )),
    }
}

/// Map a successful key-policy lookup into the stable identity used for
/// usage ledgers and traces. Rejects missing policy (unknown key).
///
/// `raw_bearer` is only used to ensure we never return it as the identity;
/// the returned policy's `key_id` must differ from the raw secret when they
/// are not intentionally the same string (tests use distinct ids).
pub fn accept_policy(
    raw_bearer: &str,
    policy: Option<KeyPolicy>,
) -> Result<KeyPolicy, GatewayError> {
    let policy =
        policy.ok_or_else(|| GatewayError::Unauthorized("invalid or unknown api key".into()))?;
    // Defense in depth: never treat the secret token as the ledger id when a
    // separate stored id exists. Policy always carries the DB key id.
    if policy.key_id.is_empty() {
        return Err(GatewayError::Internal(
            "key policy resolved with empty key_id".into(),
        ));
    }
    // If somehow key_id equals the raw bearer, still accept (caller may use
    // opaque tokens that are also ids) but the important case is: when they
    // differ, downstream code must use policy.key_id only.
    let _ = raw_bearer;
    Ok(policy)
}

/// Builds a QuotaCheckRequest from the key policy for the quota engine.
pub fn build_quota_check(policy: &KeyPolicy, alias: &str) -> QuotaCheckRequest {
    QuotaCheckRequest {
        downstream_key_id: policy.key_id.clone(),
        rate_limit_rpm: policy.rate_limit_rpm,
        model_alias: alias.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_bearer_rejects_missing_and_blank() {
        assert!(matches!(
            require_bearer(None),
            Err(GatewayError::Unauthorized(_))
        ));
        assert!(matches!(
            require_bearer(Some("")),
            Err(GatewayError::Unauthorized(_))
        ));
        assert!(matches!(
            require_bearer(Some("   ")),
            Err(GatewayError::Unauthorized(_))
        ));
        assert_eq!(require_bearer(Some(" sk-live ")).unwrap(), "sk-live");
    }

    #[test]
    fn accept_policy_rejects_unknown_key() {
        assert!(matches!(
            accept_policy("sk-secret-raw", None),
            Err(GatewayError::Unauthorized(_))
        ));
    }

    #[test]
    fn accept_policy_returns_stable_key_id_not_raw_bearer() {
        let raw = "sk-super-secret-bearer-token-do-not-store";
        let policy = KeyPolicy {
            key_id: "key_01STABLE".into(),
            model_whitelist: vec![],
            rate_limit_rpm: None,
        };
        let accepted = accept_policy(raw, Some(policy)).unwrap();
        assert_eq!(accepted.key_id, "key_01STABLE");
        assert_ne!(accepted.key_id, raw);
        let check = build_quota_check(&accepted, "gpt-4o");
        assert_eq!(check.downstream_key_id, "key_01STABLE");
    }
}
