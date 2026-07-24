//! Console API handlers — CRUD for providers, routes, downstream keys, usage, pricing.

mod common;
mod keys;
mod pricing;
mod providers;
mod quota;
mod routes_api;
mod usage;

pub use keys::{
    create_key, delete_key, get_key, get_key_secret, list_keys, update_key, CreateKeyBody,
    KeyCreateResponse, UpdateKeyBody,
};
pub use pricing::{
    delete_pricing_override, list_pricing, list_pricing_overrides, reload_pricing, sync_pricing,
    upsert_pricing_override, DeletePricingOverrideQuery, SyncPricingBody, UpsertPricingOverrideBody,
};
pub use providers::{
    create_provider, delete_provider, get_provider, get_provider_secret, list_providers,
    set_provider_secret, update_provider, CreateProviderBody, SetSecretBody, UpdateProviderBody,
};
pub use quota::{
    clear_all_cooldowns, clear_provider_cooldown, get_quota_snapshot, list_cooldowns,
    list_quota_snapshots, refresh_all_quota_snapshots, refresh_quota_snapshot, RefreshQuotaQuery,
};
pub use routes_api::{
    create_route, delete_route, get_route, list_routes, update_route, CreateRouteBody,
    UpdateRouteBody,
};
pub use usage::{list_usage, usage_summary, ListUsageQuery, UsageSummaryQuery};

#[cfg(test)]
mod tests {
    use super::common::{generate_downstream_raw_key, validate_rpm};

    #[test]
    fn rpm_validation_rejects_zero_and_negative() {
        assert!(validate_rpm(Some(-1)).is_err());
        assert!(validate_rpm(Some(0)).is_err());
        assert!(validate_rpm(Some(1)).is_ok());
        assert!(validate_rpm(Some(600)).is_ok());
        // Absent limit is fine — no rate limiting.
        assert!(validate_rpm(None).is_ok());
    }

    #[test]
    fn downstream_raw_key_shape_and_uniqueness() {
        let a = generate_downstream_raw_key();
        let b = generate_downstream_raw_key();
        // sk_ + 64 hex chars from 32 random bytes
        assert!(a.starts_with("sk_"), "{a}");
        assert_eq!(a.len(), 3 + 64, "{a}");
        assert!(
            a[3..].chars().all(|c| c.is_ascii_hexdigit()),
            "suffix must be hex: {a}"
        );
        assert_ne!(a, b, "OsRng keys must differ across calls");
    }

    #[test]
    fn create_key_source_uses_osrng_not_ulid_hash_entropy() {
        // Production key minting lives in common.rs / keys.rs after the console split.
        let common = include_str!("common.rs");
        let keys = include_str!("keys.rs");
        assert!(
            keys.contains("generate_downstream_raw_key"),
            "create_key path must mint keys via generate_downstream_raw_key"
        );
        assert!(
            common.contains("OsRng"),
            "key minting must use OsRng"
        );
        // Old anti-pattern: ULID/timestamp fed into blake3 as the token body.
        assert!(
            !common.contains("Use blake3's keyed hash on a random ULID")
                && !keys.contains("Use blake3's keyed hash on a random ULID"),
            "must not reintroduce ULID+blake3 as key entropy"
        );
    }
}
