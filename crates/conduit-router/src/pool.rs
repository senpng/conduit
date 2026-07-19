//! Provider pools for multi-account route targets (scheme B).
//!
//! A [`RouteTarget`] may reference a pool via `pool_id` and/or `pool_kind` instead
//! of a single fixed `provider_id`. Membership is resolved from the
//! [`RoutingTable`] provider catalog (by kind and/or named pool member list).
//!
//! **Pool member scheduling** uses [`PoolStrategy`] (`round_robin` | `fill_first`),
//! not multi-target `fixed`/`fallback`/`weighted`. Session affinity is applied as
//! a base layer before the pool mode (preferred pin wins when eligible).

use std::collections::HashMap;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::table::RouteTarget;

/// How pool members are chosen when no session pin applies.
///
/// Default: [`RoundRobin`](PoolStrategy::RoundRobin).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PoolStrategy {
    /// Rotate a stable cursor across eligible members (across requests).
    #[default]
    RoundRobin,
    /// Always prefer the first eligible member in stable `provider_id` order.
    FillFirst,
}

impl PoolStrategy {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "fill_first" | "fill-first" | "fillfirst" => Self::FillFirst,
            _ => Self::RoundRobin,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoundRobin => "round_robin",
            Self::FillFirst => "fill_first",
        }
    }
}

/// Process-local round-robin cursors keyed by route alias (or other schedule key).
///
/// Not persisted across restarts. Advanced only on attempt-0 picks that used
/// round-robin (no session pin).
#[derive(Debug, Default)]
pub struct PoolCursorStore {
    /// `key` → next index into the eligible set
    cursors: Mutex<HashMap<String, usize>>,
}

impl PoolCursorStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn norm_key(key: &str) -> String {
        key.trim().to_ascii_lowercase()
    }

    /// Peek the current cursor for `key` without advancing.
    pub fn peek(&self, key: &str) -> usize {
        let k = Self::norm_key(key);
        if k.is_empty() {
            return 0;
        }
        *self.cursors.lock().get(&k).unwrap_or(&0)
    }

    /// Take the current cursor index for a set of size `n`, then advance for the
    /// next request. Returns 0 when `n == 0`.
    pub fn take_and_advance(&self, key: &str, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        let k = Self::norm_key(key);
        if k.is_empty() {
            return 0;
        }
        let mut map = self.cursors.lock();
        let entry = map.entry(k).or_insert(0);
        let cur = *entry % n;
        *entry = entry.wrapping_add(1);
        cur
    }

    /// Reset cursor (tests).
    pub fn reset(&self, key: &str) {
        let k = Self::norm_key(key);
        self.cursors.lock().remove(&k);
    }
}

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

/// Result of pool member selection.
#[derive(Debug)]
pub struct PoolSelection<'a> {
    pub target: &'a RouteTarget,
    /// When true, the caller should treat this as a fresh RR advance already applied
    /// via [`PoolCursorStore::take_and_advance`] (no extra bookkeeping needed).
    pub used_round_robin: bool,
}

/// Eligible members: not in `skip`; if all skipped, fall back to full list.
pub fn eligible_members<'a>(
    members: &'a [RouteTarget],
    skip_provider_ids: Option<&std::collections::HashSet<String>>,
) -> Vec<&'a RouteTarget> {
    match skip_provider_ids {
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
    }
}

/// Stable order by `provider_id` (lexicographic) for fill-first / RR base order.
pub fn stable_member_order<'a>(members: &[&'a RouteTarget]) -> Vec<&'a RouteTarget> {
    let mut v = members.to_vec();
    v.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));
    v
}

/// Select among concrete pool members.
///
/// 1. Session affinity: if `preferred_provider_id` is eligible, it wins on attempt 0
///    and leads the retry walk.
/// 2. Else [`PoolStrategy::FillFirst`]: first eligible in stable `provider_id` order.
/// 3. Else [`PoolStrategy::RoundRobin`]: rotate with `rr_cursor` (caller supplies
///    current cursor; advances via `cursors` when provided on attempt 0).
///
/// `schedule_key` identifies the RR cursor (typically the route alias).
pub fn select_among_members<'a>(
    members: &'a [RouteTarget],
    preferred_provider_id: Option<&str>,
    skip_provider_ids: Option<&std::collections::HashSet<String>>,
    attempt_no: u32,
    mode: PoolStrategy,
    schedule_key: &str,
    cursors: Option<&PoolCursorStore>,
) -> Option<PoolSelection<'a>> {
    if members.is_empty() {
        return None;
    }

    let available = eligible_members(members, skip_provider_ids);
    if available.is_empty() {
        return None;
    }
    let ordered = stable_member_order(&available);

    // Session pin base layer.
    if let Some(pref) = preferred_provider_id.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(t) = ordered.iter().find(|t| t.provider_id == pref) {
            if attempt_no == 0 {
                return Some(PoolSelection {
                    target: *t,
                    used_round_robin: false,
                });
            }
            // Retries: pin first, then remaining stable order.
            let mut walk: Vec<&RouteTarget> = Vec::with_capacity(ordered.len());
            walk.push(*t);
            for x in &ordered {
                if x.provider_id != pref {
                    walk.push(*x);
                }
            }
            let idx = (attempt_no as usize).min(walk.len() - 1);
            return Some(PoolSelection {
                target: walk[idx],
                used_round_robin: false,
            });
        }
    }

    // No eligible pin → pool mode.
    match mode {
        PoolStrategy::FillFirst => {
            let idx = (attempt_no as usize).min(ordered.len() - 1);
            Some(PoolSelection {
                target: ordered[idx],
                used_round_robin: false,
            })
        }
        PoolStrategy::RoundRobin => {
            let n = ordered.len();
            let start = if attempt_no == 0 {
                if let Some(store) = cursors {
                    store.take_and_advance(schedule_key, n)
                } else {
                    0
                }
            } else if let Some(store) = cursors {
                // Cursor was advanced on attempt 0; previous start is (peek - 1) mod n.
                let next = store.peek(schedule_key);
                if n == 0 {
                    0
                } else {
                    next.wrapping_sub(1) % n
                }
            } else {
                0
            };
            let idx = (start + attempt_no as usize) % n;
            Some(PoolSelection {
                target: ordered[idx],
                used_round_robin: attempt_no == 0,
            })
        }
    }
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

    fn member(id: &str) -> RouteTarget {
        RouteTarget {
            provider_id: id.into(),
            model_id: "m".into(),
            provider_kind: "claude-oauth".into(),
            base_url: None,
            weight: 1,
            request_overrides: Map::new(),
            pool_id: None,
            pool_kind: None,
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
    fn session_pin_preferred_when_not_cooling() {
        let members = vec![member("a"), member("b")];
        let pick = select_among_members(
            &members,
            Some("b"),
            None,
            0,
            PoolStrategy::RoundRobin,
            "alias",
            None,
        )
        .unwrap();
        assert_eq!(pick.target.provider_id, "b");
    }

    #[test]
    fn session_pin_skipped_when_cooling_falls_to_fill_first() {
        let members = vec![member("a"), member("b")];
        let mut skip = std::collections::HashSet::new();
        skip.insert("b".into());
        let pick = select_among_members(
            &members,
            Some("b"),
            Some(&skip),
            0,
            PoolStrategy::FillFirst,
            "alias",
            None,
        )
        .unwrap();
        // fill-first among remaining: stable order → "a"
        assert_eq!(pick.target.provider_id, "a");
    }

    #[test]
    fn fill_first_always_first_eligible_stable_order() {
        // Unstable input order; stable sort by provider_id → a, b, c
        let members = vec![member("c"), member("a"), member("b")];
        for _ in 0..3 {
            let pick = select_among_members(
                &members,
                None,
                None,
                0,
                PoolStrategy::FillFirst,
                "alias",
                None,
            )
            .unwrap();
            assert_eq!(pick.target.provider_id, "a");
        }
        // attempt 1 walks to next
        let pick1 = select_among_members(
            &members,
            None,
            None,
            1,
            PoolStrategy::FillFirst,
            "alias",
            None,
        )
        .unwrap();
        assert_eq!(pick1.target.provider_id, "b");
    }

    #[test]
    fn round_robin_advances_cursor_across_requests() {
        let members = vec![member("a"), member("b"), member("c")];
        let cursors = PoolCursorStore::new();
        let mut seen = Vec::new();
        for _ in 0..3 {
            let pick = select_among_members(
                &members,
                None,
                None,
                0,
                PoolStrategy::RoundRobin,
                "claude",
                Some(&cursors),
            )
            .unwrap();
            seen.push(pick.target.provider_id.clone());
        }
        // Stable order a,b,c — RR should rotate
        assert_eq!(seen, vec!["a", "b", "c"]);
        // fourth wraps
        let pick = select_among_members(
            &members,
            None,
            None,
            0,
            PoolStrategy::RoundRobin,
            "claude",
            Some(&cursors),
        )
        .unwrap();
        assert_eq!(pick.target.provider_id, "a");
    }

    #[test]
    fn round_robin_pin_does_not_advance_for_other_sessions_fairness_ok() {
        // Pin wins without consuming RR fairness for that pick — cursor only
        // advances on non-pin RR picks (tested via successive non-pin picks).
        let members = vec![member("a"), member("b")];
        let cursors = PoolCursorStore::new();
        let p = select_among_members(
            &members,
            Some("b"),
            None,
            0,
            PoolStrategy::RoundRobin,
            "r",
            Some(&cursors),
        )
        .unwrap();
        assert_eq!(p.target.provider_id, "b");
        assert!(!p.used_round_robin);
        // Cursor still 0 → next non-pin pick is "a"
        let p2 = select_among_members(
            &members,
            None,
            None,
            0,
            PoolStrategy::RoundRobin,
            "r",
            Some(&cursors),
        )
        .unwrap();
        assert_eq!(p2.target.provider_id, "a");
    }

    #[test]
    fn no_session_no_pin_uses_mode_only() {
        let members = vec![member("z"), member("a")];
        let pick = select_among_members(
            &members,
            None, // no session pin
            None,
            0,
            PoolStrategy::FillFirst,
            "x",
            None,
        )
        .unwrap();
        assert_eq!(pick.target.provider_id, "a");
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

    #[test]
    fn pool_strategy_parse() {
        assert_eq!(PoolStrategy::parse("round_robin"), PoolStrategy::RoundRobin);
        assert_eq!(PoolStrategy::parse("fill_first"), PoolStrategy::FillFirst);
        assert_eq!(PoolStrategy::parse("fill-first"), PoolStrategy::FillFirst);
        assert_eq!(PoolStrategy::parse("unknown"), PoolStrategy::RoundRobin);
    }
}
