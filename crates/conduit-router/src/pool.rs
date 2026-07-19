//! Provider pools for multi-account route targets (scheme B).
//!
//! A [`RouteTarget`] may reference a pool via `pool_id` and/or `pool_kind` instead
//! of a single fixed `provider_id`. Membership is resolved from the
//! [`RoutingTable`] provider catalog (by kind and/or named pool member list).

use serde::{Deserialize, Serialize};

use crate::table::RouteTarget;

/// One concrete provider available for pool expansion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCatalogEntry {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default = "default_weight")]
    pub weight: u32,
}

fn default_weight() -> u32 {
    1
}

/// Named pool definition.
///
/// - If `members` is non-empty, only those provider ids are included.
/// - Else if `kind` is set, all catalog providers of that kind are included.
/// - `pool_id` on a target may also match a provider **kind** name when no
///   explicit named pool exists (auto kind-pool).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NamedPool {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub members: Vec<String>,
}

impl RouteTarget {
    /// True when this target should expand to a provider pool.
    pub fn is_pool_target(&self) -> bool {
        self.pool_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some()
            || self
                .pool_kind
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_some()
    }
}

/// Expand a pool (or single) target into concrete provider targets.
///
/// Template fields (`model_id`, `request_overrides`, route-level weight default)
/// are copied onto each expanded member. Member `weight` comes from the catalog.
pub fn expand_route_target(
    template: &RouteTarget,
    providers: &[ProviderCatalogEntry],
    named_pools: &std::collections::HashMap<String, NamedPool>,
) -> Result<Vec<RouteTarget>, String> {
    if !template.is_pool_target() {
        if template.provider_id.trim().is_empty() {
            return Err("route target has empty provider_id and no pool reference".into());
        }
        return Ok(vec![template.clone()]);
    }

    let members = resolve_pool_members(
        template.pool_id.as_deref(),
        template.pool_kind.as_deref(),
        providers,
        named_pools,
    )?;

    if members.is_empty() {
        let pref = template
            .pool_id
            .as_deref()
            .or(template.pool_kind.as_deref())
            .unwrap_or("?");
        return Err(format!("provider pool '{pref}' has no members"));
    }

    let expanded = members
        .into_iter()
        .map(|m| RouteTarget {
            provider_id: m.id.clone(),
            model_id: template.model_id.clone(),
            provider_kind: m.kind.clone(),
            base_url: m.base_url.clone().or_else(|| template.base_url.clone()),
            weight: if m.weight > 0 {
                m.weight
            } else {
                template.weight
            },
            request_overrides: template.request_overrides.clone(),
            pool_id: None,
            pool_kind: None,
        })
        .collect();
    Ok(expanded)
}

/// Resolve catalog entries for a pool reference.
pub fn resolve_pool_members<'a>(
    pool_id: Option<&str>,
    pool_kind: Option<&str>,
    providers: &'a [ProviderCatalogEntry],
    named_pools: &std::collections::HashMap<String, NamedPool>,
) -> Result<Vec<&'a ProviderCatalogEntry>, String> {
    let pool_id = pool_id.map(str::trim).filter(|s| !s.is_empty());
    let pool_kind = pool_kind.map(str::trim).filter(|s| !s.is_empty());

    // Explicit kind filter wins when set.
    if let Some(kind) = pool_kind {
        return Ok(providers_of_kind(providers, kind));
    }

    if let Some(pid) = pool_id {
        if let Some(np) = named_pools.get(pid) {
            if !np.members.is_empty() {
                let set: std::collections::HashSet<&str> =
                    np.members.iter().map(|s| s.as_str()).collect();
                let list: Vec<_> = providers
                    .iter()
                    .filter(|p| set.contains(p.id.as_str()))
                    .collect();
                return Ok(list);
            }
            if let Some(ref k) = np.kind {
                return Ok(providers_of_kind(providers, k));
            }
        }
        // Auto kind-pool: pool_id names a provider kind.
        let by_kind = providers_of_kind(providers, pid);
        if !by_kind.is_empty() {
            return Ok(by_kind);
        }
        return Err(format!("unknown provider pool '{pid}'"));
    }

    Err("pool target requires pool_id or pool_kind".into())
}

fn providers_of_kind<'a>(
    providers: &'a [ProviderCatalogEntry],
    kind: &str,
) -> Vec<&'a ProviderCatalogEntry> {
    let kind_l = kind.to_ascii_lowercase();
    providers
        .iter()
        .filter(|p| p.kind.eq_ignore_ascii_case(&kind_l))
        .collect()
}

/// Build auto kind-pools for every distinct provider kind (id = kind string).
pub fn auto_kind_pools(
    providers: &[ProviderCatalogEntry],
) -> std::collections::HashMap<String, NamedPool> {
    let mut map = std::collections::HashMap::new();
    for p in providers {
        let k = p.kind.clone();
        map.entry(k.clone()).or_insert_with(|| NamedPool {
            kind: Some(k),
            members: vec![],
        });
    }
    map
}

/// Select among concrete targets: sticky pin if available and not cooling,
/// else weighted/RR-style pick using `seed` for attempt 0, walk order for retries.
pub fn select_among_members<'a>(
    members: &'a [RouteTarget],
    preferred_provider_id: Option<&str>,
    skip_provider_ids: Option<&std::collections::HashSet<String>>,
    attempt_no: u32,
    seed: u64,
) -> Option<&'a RouteTarget> {
    if members.is_empty() {
        return None;
    }

    // Available = not cooling; if all cooling, fall back to full list.
    let available: Vec<&RouteTarget> = match skip_provider_ids {
        Some(skip) if !skip.is_empty() => {
            let v: Vec<_> = members
                .iter()
                .filter(|t| !skip.contains(&t.provider_id))
                .collect();
            if v.is_empty() {
                members.iter().collect()
            } else {
                v
            }
        }
        _ => members.iter().collect(),
    };

    // Sticky pin: prefer if in available set.
    if let Some(pref) = preferred_provider_id.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(t) = available.iter().find(|t| t.provider_id == pref) {
            if attempt_no == 0 {
                return Some(*t);
            }
            // Retries: walk sticky order (pin first, then others).
            let mut order: Vec<&RouteTarget> = Vec::with_capacity(available.len());
            order.push(*t);
            for x in &available {
                if x.provider_id != pref {
                    order.push(*x);
                }
            }
            let idx = (attempt_no as usize).min(order.len() - 1);
            return Some(order[idx]);
        }
    }

    // No pin: equal-weight RR via seed on attempt 0; then walk remainder.
    if available.len() == 1 {
        return Some(available[0]);
    }
    let first_idx = (seed as usize) % available.len();
    if attempt_no == 0 {
        return Some(available[first_idx]);
    }
    let mut order = Vec::with_capacity(available.len());
    order.push(available[first_idx]);
    for (i, t) in available.iter().enumerate() {
        if i != first_idx {
            order.push(*t);
        }
    }
    let idx = (attempt_no as usize).min(order.len() - 1);
    Some(order[idx])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn entry(id: &str, kind: &str) -> ProviderCatalogEntry {
        ProviderCatalogEntry {
            id: id.into(),
            kind: kind.into(),
            base_url: Some(format!("https://{id}.example")),
            weight: 1,
        }
    }

    fn pool_template(kind: &str, model: &str) -> RouteTarget {
        RouteTarget {
            provider_id: String::new(),
            model_id: model.into(),
            provider_kind: kind.into(),
            base_url: None,
            weight: 1,
            request_overrides: Map::new(),
            pool_id: None,
            pool_kind: Some(kind.into()),
        }
    }

    #[test]
    fn expand_by_kind() {
        let providers = vec![
            entry("c1", "claude-oauth"),
            entry("c2", "claude-oauth"),
            entry("o1", "openai"),
        ];
        let pools = auto_kind_pools(&providers);
        let t = pool_template("claude-oauth", "claude-sonnet");
        let exp = expand_route_target(&t, &providers, &pools).unwrap();
        assert_eq!(exp.len(), 2);
        assert_eq!(exp[0].provider_id, "c1");
        assert_eq!(exp[0].model_id, "claude-sonnet");
        assert_eq!(exp[1].provider_id, "c2");
    }

    #[test]
    fn expand_named_members() {
        let providers = vec![entry("a", "claude-oauth"), entry("b", "claude-oauth")];
        let mut pools = std::collections::HashMap::new();
        pools.insert(
            "team".into(),
            NamedPool {
                kind: None,
                members: vec!["b".into()],
            },
        );
        let mut t = pool_template("claude-oauth", "m");
        t.pool_kind = None;
        t.pool_id = Some("team".into());
        let exp = expand_route_target(&t, &providers, &pools).unwrap();
        assert_eq!(exp.len(), 1);
        assert_eq!(exp[0].provider_id, "b");
    }

    #[test]
    fn empty_pool_errors() {
        let providers = vec![entry("o1", "openai")];
        let pools = auto_kind_pools(&providers);
        let t = pool_template("claude-oauth", "m");
        let err = expand_route_target(&t, &providers, &pools).unwrap_err();
        assert!(err.contains("no members"), "{err}");
    }

    #[test]
    fn sticky_pin_preferred_when_not_cooling() {
        let members = vec![
            RouteTarget {
                provider_id: "a".into(),
                model_id: "m".into(),
                provider_kind: "claude-oauth".into(),
                base_url: None,
                weight: 1,
                request_overrides: Map::new(),
                pool_id: None,
                pool_kind: None,
            },
            RouteTarget {
                provider_id: "b".into(),
                model_id: "m".into(),
                provider_kind: "claude-oauth".into(),
                base_url: None,
                weight: 1,
                request_overrides: Map::new(),
                pool_id: None,
                pool_kind: None,
            },
        ];
        let pick = select_among_members(&members, Some("b"), None, 0, 0).unwrap();
        assert_eq!(pick.provider_id, "b");
    }

    #[test]
    fn sticky_pin_skipped_when_cooling() {
        let members = vec![
            RouteTarget {
                provider_id: "a".into(),
                model_id: "m".into(),
                provider_kind: "k".into(),
                base_url: None,
                weight: 1,
                request_overrides: Map::new(),
                pool_id: None,
                pool_kind: None,
            },
            RouteTarget {
                provider_id: "b".into(),
                model_id: "m".into(),
                provider_kind: "k".into(),
                base_url: None,
                weight: 1,
                request_overrides: Map::new(),
                pool_id: None,
                pool_kind: None,
            },
        ];
        let mut skip = std::collections::HashSet::new();
        skip.insert("b".into());
        let pick = select_among_members(&members, Some("b"), Some(&skip), 0, 0).unwrap();
        assert_eq!(pick.provider_id, "a");
    }

    #[test]
    fn single_provider_target_unchanged() {
        let providers = vec![entry("p1", "openai")];
        let pools = auto_kind_pools(&providers);
        let t = RouteTarget {
            provider_id: "p1".into(),
            model_id: "gpt".into(),
            provider_kind: "openai".into(),
            base_url: None,
            weight: 1,
            request_overrides: Map::new(),
            pool_id: None,
            pool_kind: None,
        };
        let exp = expand_route_target(&t, &providers, &pools).unwrap();
        assert_eq!(exp.len(), 1);
        assert_eq!(exp[0].provider_id, "p1");
    }
}
