//! Overlay form / wizard state for write operations.

mod common;
mod key;
mod oauth;
mod pricing;
mod provider;
mod route;

pub use common::{current_period, shift_period, ConfirmAction};
pub use key::KeyForm;
pub use oauth::OauthFlow;
#[allow(unused_imports)] // public API surface for callers / future TUI wiring
pub use oauth::OAUTH_KINDS;
pub use pricing::PricingOverrideForm;
pub use provider::{ProviderAddChooser, ProviderForm, ProviderFormKind, PROVIDER_ADD_OPTIONS};
#[allow(unused_imports)]
pub use provider::PROVIDER_KINDS;
pub use route::{summarize_route_targets, RouteWizard};
#[allow(unused_imports)] // public API surface for callers / tests
pub use route::{TargetBinding, TargetDraft};

#[cfg(test)]
mod tests {
    use crate::dto::{KeyView, ProviderView, RouteView};
    use super::*;
    use super::pricing::format_rate;

    fn pv(id: &str, name: &str, kind: &str) -> ProviderView {
        ProviderView {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            base_url: format!("https://{id}.example"),
            upstream_key_ref: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn route_wizard_builds_targets() {
        let mut w = RouteWizard::create(vec![pv("p1", "oai", "openai")]);
        w.match_alias.value = "gpt-4o".into();
        w.targets[0].model_id.value = "gpt-4o".into();
        let body = w.to_body().unwrap();
        assert_eq!(body.match_alias, "gpt-4o");
        assert_eq!(body.strategy, "fixed");
        let arr = body.targets.as_array().unwrap();
        assert_eq!(arr[0]["provider_id"], "p1");
        assert!(arr[0].get("pool_kind").is_none());
    }

    #[test]
    fn route_wizard_pool_kind_target() {
        let providers = vec![
            pv("c1", "claude-a", "claude-oauth"),
            pv("c2", "claude-b", "claude-oauth"),
            pv("o1", "oai", "openai"),
        ];
        let mut w = RouteWizard::create(providers);
        w.match_alias.value = "claude".into();
        w.targets[0].binding = TargetBinding::PoolKind {
            kind: "claude-oauth".into(),
        };
        w.targets[0].model_id.value = "claude-sonnet-4".into();
        // Pool targets use round_robin / fill_first (strategy_idx 0 / 1), not weighted.
        w.strategy_idx = 0;
        let body = w.to_body().unwrap();
        assert_eq!(body.strategy, "round_robin");
        let t = &body.targets.as_array().unwrap()[0];
        assert_eq!(t["pool_kind"], "claude-oauth");
        assert_eq!(t["provider_kind"], "claude-oauth");
        assert_eq!(t["model_id"], "claude-sonnet-4");
        assert!(
            t.get("provider_id").is_none(),
            "empty provider_id should be omitted: {t}"
        );
        assert!(t.get("base_url").is_none());
    }

    #[test]
    fn route_wizard_cycles_strategy_and_binding() {
        let mut w = RouteWizard::create(vec![
            pv("c1", "a", "claude-oauth"),
            pv("c2", "b", "claude-oauth"),
        ]);
        assert_eq!(w.strategy(), "fixed");
        w.cycle_strategy();
        assert_eq!(w.strategy(), "fallback");
        w.cycle_strategy();
        assert_eq!(w.strategy(), "weighted");
        w.cycle_strategy();
        assert_eq!(w.strategy(), "fixed");

        // 2 singles + 1 pool kind
        assert_eq!(w.binding_options().len(), 3);
        w.cycle_provider(); // c1 -> c2
        w.cycle_provider(); // c2 -> pool claude-oauth
        assert!(matches!(
            &w.targets[0].binding,
            TargetBinding::PoolKind { kind } if kind == "claude-oauth"
        ));
    }

    #[test]
    fn provider_edit_body_uses_name_and_base_url_only() {
        let p = pv("id1", "old", "openai");
        let mut f = ProviderForm::edit(&p);
        assert_eq!(f.fields.len(), 2, "edit form must not include kind field");
        f.fields[0].value = "new".into();
        f.fields[1].value = "https://new.example".into();
        let body = f.to_update_body().unwrap();
        assert_eq!(body.name.as_deref(), Some("new"));
        assert_eq!(body.base_url.as_deref(), Some("https://new.example"));
    }

    fn kv(
        id: &str,
        name: &str,
        rpm: Option<i64>,
        whitelist: serde_json::Value,
        enabled: bool,
    ) -> KeyView {
        KeyView {
            id: id.into(),
            name: name.into(),
            model_whitelist: whitelist,
            rate_limit_rpm: rpm,
            enabled,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn key_form_create_body() {
        let mut f = KeyForm::create();
        assert!(!f.is_edit());
        f.fields[0].value = "ops".into();
        f.fields[1].value = "120".into();
        f.fields[2].value = "gpt-4o, claude-sonnet".into();
        let body = f.to_create_body().unwrap();
        assert_eq!(body.name, "ops");
        assert_eq!(body.rate_limit_rpm, Some(120));
        assert_eq!(
            body.model_whitelist.as_deref(),
            Some(["gpt-4o".into(), "claude-sonnet".into()].as_slice())
        );
    }

    #[test]
    fn key_form_edit_prefills_and_updates() {
        let k = kv(
            "k1",
            "old",
            Some(60),
            serde_json::json!(["gpt-4o"]),
            true,
        );
        let mut f = KeyForm::edit(&k);
        assert_eq!(f.edit_id.as_deref(), Some("k1"));
        assert_eq!(f.fields.len(), 4);
        assert_eq!(f.fields[0].value, "old");
        assert_eq!(f.fields[1].value, "60");
        assert_eq!(f.fields[2].value, "gpt-4o");
        assert_eq!(f.fields[3].value, "true");

        f.fields[0].value = "renamed".into();
        f.fields[1].value = String::new(); // unlimited
        f.fields[2].value = String::new(); // allow all
        f.cycle_enabled();
        assert_eq!(f.fields[3].value, "false");

        let body = f.to_update_body().unwrap();
        assert_eq!(body.name.as_deref(), Some("renamed"));
        assert_eq!(body.rate_limit_rpm, None);
        assert_eq!(body.model_whitelist.as_deref(), Some([].as_slice()));
        assert_eq!(body.enabled, Some(false));
    }

    #[test]
    fn key_form_rejects_non_positive_rpm() {
        let mut f = KeyForm::create();
        f.fields[0].value = "x".into();
        f.fields[1].value = "0".into();
        assert!(f.to_create_body().unwrap_err().contains("positive"));
    }

    #[test]
    fn route_edit_preserves_provider_id_when_list_loads_later() {
        let route = RouteView {
            id: "r1".into(),
            match_alias: "gpt-4o".into(),
            strategy: "fixed".into(),
            targets_json: r#"[{"provider_id":"p2","model_id":"m","provider_kind":"openai"}]"#
                .into(),
            retry_policy_json: String::new(),
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let mut w = RouteWizard::edit(&route, vec![]);
        assert_eq!(
            w.targets[0].binding,
            TargetBinding::Provider { id: "p2".into() }
        );
        w.set_providers(vec![pv("p1", "a", "openai"), pv("p2", "b", "openai")]);
        assert_eq!(
            w.targets[0].binding,
            TargetBinding::Provider { id: "p2".into() }
        );
        w.targets[0].model_id.value = "m".into();
        let body = w.to_body().unwrap();
        assert_eq!(body.targets[0]["provider_id"], "p2");
    }

    #[test]
    fn route_edit_preserves_pool_kind() {
        let route = RouteView {
            id: "r1".into(),
            match_alias: "sonnet".into(),
            // Pool routes store pool schedule (round_robin / fill_first), not weighted.
            strategy: "round_robin".into(),
            targets_json:
                r#"[{"pool_kind":"claude-oauth","model_id":"claude-sonnet-4","provider_kind":"claude-oauth"}]"#
                    .into(),
            retry_policy_json: String::new(),
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let w = RouteWizard::edit(
            &route,
            vec![pv("c1", "a", "claude-oauth"), pv("c2", "b", "claude-oauth")],
        );
        assert_eq!(w.strategy(), "round_robin");
        assert_eq!(
            w.targets[0].binding,
            TargetBinding::PoolKind {
                kind: "claude-oauth".into()
            }
        );
        let body = w.to_body().unwrap();
        assert_eq!(body.targets[0]["pool_kind"], "claude-oauth");
    }

    #[test]
    fn summarize_route_targets_shows_pool() {
        let providers = vec![pv("c1", "a", "claude-oauth"), pv("c2", "b", "claude-oauth")];
        let s = summarize_route_targets(
            r#"[{"pool_kind":"claude-oauth","model_id":"m"}]"#,
            &providers,
        );
        assert!(s.contains("pool:claude-oauth×2"), "{s}");
        assert!(s.contains("→m"), "{s}");
    }

    #[test]
    fn shift_period_wraps_year() {
        assert_eq!(shift_period("2026-01", -1), "2025-12");
        assert_eq!(shift_period("2025-12", 1), "2026-01");
    }

    #[test]
    fn shift_period_multi_month_and_malformed() {
        assert_eq!(shift_period("2026-03", -5), "2025-10");
        assert_eq!(shift_period("2026-11", 3), "2027-02");
        // Malformed / out-of-range month falls back to the current month
        // instead of looping the old `while` normalization.
        assert_eq!(shift_period("2026-13", 1), current_period());
        assert_eq!(shift_period("garbage", 1), current_period());
    }

    #[test]
    fn format_rate_keeps_sub_micro_precision() {
        // 6-decimal formatting would render this as "0"; keep it visible so
        // editing an override never silently zeroes a tiny rate.
        assert_eq!(format_rate(0.0000005), "0.0000005");
        assert_eq!(format_rate(0.0), "0");
        assert_eq!(format_rate(1.0), "1");
        assert_eq!(format_rate(3.5), "3.5");
    }
}
