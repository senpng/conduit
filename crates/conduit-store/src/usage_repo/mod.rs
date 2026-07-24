//! Per-request usage ledger.
//!
//! Every completed request with non-zero tokens or cost is written here.
//!
//! Calendar day / month rollups default to **UTC**, but accept a client
//! `tz_offset_minutes` (minutes east of UTC) so TUI charts follow local days.

mod list;
mod map;
mod sql;
mod summary;
mod types;
mod write;

use sqlx::SqlitePool;

pub use map::{new_usage_attempt, new_usage_record};
pub use sql::clamp_tz_offset_minutes;
pub use types::{
    UsageDayModelRow, UsageDayRow, UsageKeyModelRow, UsageListOpts, UsageListPage, UsageListSort,
    UsageModelRow, UsageOutcomeSummary, UsageProviderRow, UsageSummaryRow,
};

pub struct UsageRepo<'a> {
    pub(crate) pool: &'a SqlitePool,
}

impl<'a> UsageRepo<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_db;

    #[tokio::test]
    async fn insert_and_list() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        let row = new_usage_record(
            "req-1",
            Some("dk1".into()),
            Some("gpt".into()),
            Some("p1".into()),
            Some("openai".into()),
            Some("gpt-4o".into()),
            10,
            5,
            15,
            0,
            0,
            0,
            0.012,
            false,
        );
        repo.insert(&row).await.unwrap();

        let listed = repo.list(10, None, None).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].request_id, "req-1");
        assert!((listed[0].cost_usd - 0.012).abs() < 1e-9);

        let by_key = repo.list(10, Some("dk1"), None).await.unwrap();
        assert_eq!(by_key.len(), 1);
        let empty = repo.list(10, Some("other"), None).await.unwrap();
        assert!(empty.is_empty());

        let page = repo
            .list_page(UsageListOpts {
                limit: 10,
                offset: 0,
                key_id: None,
                period: None,
                q: Some("gpt-4o"),
                sort: UsageListSort::Date,
                        tz_offset_minutes: 0,
                        })
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.rows.len(), 1);
    }

    #[tokio::test]
    async fn list_page_offset_and_sort() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        for (i, (model, cost, toks)) in [
            ("cheap", 0.01, 10u32),
            ("mid", 0.50, 100),
            ("pricey", 2.00, 50),
        ]
        .into_iter()
        .enumerate()
        {
            let mut row = new_usage_record(
                &format!("req-{i}"),
                Some("dk1".into()),
                None,
                None,
                Some("openai".into()),
                Some(model.into()),
                toks,
                0,
                toks,
                0,
                0,
                0,
                cost,
                false,
            );
            // Distinct timestamps so date sort is stable.
            row.ts = format!("2026-07-{:02}T12:00:00Z", i + 1);
            repo.insert(&row).await.unwrap();
        }

        let by_cost = repo
            .list_page(UsageListOpts {
                limit: 2,
                offset: 0,
                key_id: None,
                period: Some("2026-07"),
                q: None,
                sort: UsageListSort::Cost,
                        tz_offset_minutes: 0,
                        })
            .await
            .unwrap();
        assert_eq!(by_cost.total, 3);
        assert_eq!(by_cost.rows.len(), 2);
        assert_eq!(by_cost.rows[0].model_id.as_deref(), Some("pricey"));
        assert_eq!(by_cost.rows[1].model_id.as_deref(), Some("mid"));

        let page2 = repo
            .list_page(UsageListOpts {
                limit: 2,
                offset: 2,
                key_id: None,
                period: Some("2026-07"),
                q: None,
                sort: UsageListSort::Cost,
                        tz_offset_minutes: 0,
                        })
            .await
            .unwrap();
        assert_eq!(page2.total, 3);
        assert_eq!(page2.rows.len(), 1);
        assert_eq!(page2.rows[0].model_id.as_deref(), Some("cheap"));

        let q = repo
            .list_page(UsageListOpts {
                limit: 10,
                offset: 0,
                key_id: None,
                period: None,
                q: Some("MID"),
                sort: UsageListSort::Date,
                        tz_offset_minutes: 0,
                        })
            .await
            .unwrap();
        assert_eq!(q.total, 1);
        assert_eq!(q.rows[0].model_id.as_deref(), Some("mid"));
    }

    #[tokio::test]
    async fn list_filters_by_period() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        let mut a = new_usage_record(
            "req-a",
            Some("dk1".into()),
            None,
            None,
            None,
            Some("m1".into()),
            1,
            1,
            2,
            0,
            0,
            0,
            0.01,
            false,
        );
        a.ts = "2026-06-15T12:00:00Z".into();
        repo.insert(&a).await.unwrap();

        let mut b = new_usage_record(
            "req-b",
            Some("dk1".into()),
            None,
            None,
            None,
            Some("m1".into()),
            1,
            1,
            2,
            0,
            0,
            0,
            0.02,
            false,
        );
        b.ts = "2026-07-01T08:00:00Z".into();
        repo.insert(&b).await.unwrap();

        let july = repo.list(10, None, Some("2026-07")).await.unwrap();
        assert_eq!(july.len(), 1);
        assert_eq!(july[0].request_id, "req-b");

        let june = repo.list(10, None, Some("2026-06")).await.unwrap();
        assert_eq!(june.len(), 1);
        assert_eq!(june[0].request_id, "req-a");

        let all = repo.list(10, None, None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn summary_period_groups_by_key() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        let mut a = new_usage_record(
            "r1",
            Some("k1".into()),
            None,
            None,
            None,
            None,
            1,
            1,
            2,
            0,
            0,
            0,
            1.0,
            false,
        );
        a.ts = "2026-07-01T00:00:00Z".into();
        let mut b = new_usage_record(
            "r2",
            Some("k1".into()),
            None,
            None,
            None,
            None,
            2,
            2,
            4,
            0,
            0,
            0,
            2.5,
            true,
        );
        b.ts = "2026-07-15T12:00:00Z".into();
        let mut c = new_usage_record(
            "r3",
            Some("k2".into()),
            None,
            None,
            None,
            None,
            1,
            0,
            1,
            0,
            0,
            0,
            0.5,
            false,
        );
        c.ts = "2026-07-20T00:00:00Z".into();
        // Outside period
        let mut d = new_usage_record(
            "r4",
            Some("k1".into()),
            None,
            None,
            None,
            None,
            1,
            0,
            1,
            0,
            0,
            0,
            9.0,
            false,
        );
        d.ts = "2026-06-01T00:00:00Z".into();

        for r in [&a, &b, &c, &d] {
            repo.insert(r).await.unwrap();
        }

        let sum = repo.summary_period("2026-07", 0).await.unwrap();
        assert_eq!(sum.len(), 2);
        let k1 = sum.iter().find(|s| s.downstream_key_id == "k1").unwrap();
        assert_eq!(k1.request_count, 2);
        assert!((k1.total_usd - 3.5).abs() < 1e-9);
        assert_eq!(k1.total_tokens, 6);
        let k2 = sum.iter().find(|s| s.downstream_key_id == "k2").unwrap();
        assert_eq!(k2.request_count, 1);
        assert!((k2.total_usd - 0.5).abs() < 1e-9);

        // Lifetime includes the June outlier (k1 +$9).
        let all = repo.summary_period("all", 0).await.unwrap();
        assert_eq!(all.len(), 2);
        let k1_all = all.iter().find(|s| s.downstream_key_id == "k1").unwrap();
        assert_eq!(k1_all.request_count, 3);
        assert!((k1_all.total_usd - 12.5).abs() < 1e-9);
        assert_eq!(k1_all.total_tokens, 7);

        let by_day = repo.summary_by_day("2026-07", None, 0).await.unwrap();
        assert_eq!(by_day.len(), 3); // 01, 15, 20
        assert_eq!(by_day[0].day, "2026-07-01");
        assert!((by_day[0].total_usd - 1.0).abs() < 1e-9);
        let day15 = by_day.iter().find(|d| d.day == "2026-07-15").unwrap();
        assert!((day15.total_usd - 2.5).abs() < 1e-9);

        let k1_days = repo.summary_by_day("2026-07", Some("k1"), 0).await.unwrap();
        assert_eq!(k1_days.len(), 2);
        assert!(k1_days.iter().all(|d| d.day.starts_with("2026-07")));

        // Alias rollup (rows above have no alias — label falls back to "(unknown)")
        let by_model = repo.summary_by_model("2026-07", None, 0).await.unwrap();
        assert!(!by_model.is_empty());
        let unknown = by_model.iter().find(|m| m.label == "(unknown)").unwrap();
        assert_eq!(unknown.request_count, 3);
        assert!((unknown.total_usd - 4.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn summary_by_day_respects_tz_offset() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);
        // UTC 2026-07-31 20:00 → Asia/Shanghai (+480) is 2026-08-01 04:00
        let mut row = new_usage_record(
            "r-tz",
            Some("k1".into()),
            None,
            None,
            None,
            None,
            1,
            0,
            1,
            0,
            0,
            0,
            1.0,
            false,
        );
        row.ts = "2026-07-31T20:00:00Z".into();
        repo.insert(&row).await.unwrap();

        let utc_days = repo.summary_by_day("2026-07", None, 0).await.unwrap();
        assert_eq!(utc_days.len(), 1);
        assert_eq!(utc_days[0].day, "2026-07-31");

        let sh_days = repo.summary_by_day("2026-08", None, 480).await.unwrap();
        assert_eq!(sh_days.len(), 1);
        assert_eq!(sh_days[0].day, "2026-08-01");

        let sh_july = repo.summary_by_day("2026-07", None, 480).await.unwrap();
        assert!(sh_july.is_empty());
    }

    #[tokio::test]
    async fn zero_consumption_success_still_inserts() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);
        let mut row = new_usage_record(
            "req-zero",
            Some("dk1".into()),
            Some("gpt".into()),
            Some("p1".into()),
            Some("openai".into()),
            Some("gpt-4o".into()),
            0,
            0,
            0,
            0,
            0,
            0,
            0.0,
            false,
        );
        row.status = "ok".into();
        row.duration_ms = Some(12);
        repo.insert(&row).await.unwrap();
        let listed = repo.list(10, None, None).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, "ok");
        assert_eq!(listed[0].duration_ms, Some(12));
        assert_eq!(listed[0].total_tokens, 0);
    }

    #[tokio::test]
    async fn terminal_error_and_attempts_insert() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        let mut main = new_usage_record(
            "req-err",
            Some("dk1".into()),
            Some("gpt".into()),
            Some("p2".into()),
            Some("openai".into()),
            Some("gpt-4o".into()),
            0,
            0,
            0,
            0,
            0,
            0,
            0.0,
            false,
        );
        main.status = "error".into();
        main.error_class = Some("rate_limited".into());
        main.http_status = Some(429);
        main.duration_ms = Some(80);
        main.attempt_no = 1;
        main.attempt_count = 2;
        main.route_strategy = Some("fallback".into());
        main.ts = "2026-07-15T10:00:00Z".into();
        repo.insert(&main).await.unwrap();

        let a0 = new_usage_attempt(
            "req-err",
            0,
            Some("p1".into()),
            Some("openai".into()),
            Some("gpt-4o".into()),
            "error",
            Some("rate_limited".into()),
            Some(429),
            Some(30),
            None,
            Some("initial".into()),
        );
        let a1 = new_usage_attempt(
            "req-err",
            1,
            Some("p2".into()),
            Some("openai".into()),
            Some("gpt-4o".into()),
            "error",
            Some("rate_limited".into()),
            Some(429),
            Some(50),
            None,
            Some("retry".into()),
        );
        repo.insert_attempt(&a0).await.unwrap();
        repo.insert_attempt(&a1).await.unwrap();

        let attempts = repo.list_attempts("req-err").await.unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].provider_id.as_deref(), Some("p1"));
        assert_eq!(attempts[1].provider_id.as_deref(), Some("p2"));
        assert_eq!(attempts[0].status, "error");

        let outcome = repo.summary_outcome("2026-07", None, 0).await.unwrap();
        assert_eq!(outcome.request_count, 1);
        assert_eq!(outcome.success_count, 0);
        assert!((outcome.success_rate - 0.0).abs() < 1e-12);

        let by_p = repo.summary_by_provider("2026-07", None, 0).await.unwrap();
        assert_eq!(by_p.len(), 1);
        assert_eq!(by_p[0].provider_id, "p2");
        assert!((by_p[0].success_rate - 0.0).abs() < 1e-12);
    }

    #[tokio::test]
    async fn summary_outcome_and_by_provider_success_rate() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        let mut ok = new_usage_record(
            "r-ok",
            Some("k1".into()),
            None,
            Some("prov-a".into()),
            Some("openai".into()),
            Some("m".into()),
            1,
            1,
            2,
            0,
            0,
            0,
            0.1,
            true,
        );
        ok.ts = "2026-07-10T00:00:00Z".into();
        ok.status = "ok".into();
        ok.ttfb_ms = Some(40);
        ok.duration_ms = Some(100);
        repo.insert(&ok).await.unwrap();

        let mut err = new_usage_record(
            "r-err",
            Some("k1".into()),
            None,
            Some("prov-a".into()),
            Some("openai".into()),
            Some("m".into()),
            0,
            0,
            0,
            0,
            0,
            0,
            0.0,
            false,
        );
        err.ts = "2026-07-11T00:00:00Z".into();
        err.status = "error".into();
        err.error_class = Some("upstream_5xx".into());
        err.duration_ms = Some(20);
        repo.insert(&err).await.unwrap();

        let mut ok_b = new_usage_record(
            "r-ok-b",
            Some("k1".into()),
            None,
            Some("prov-b".into()),
            Some("anthropic".into()),
            Some("m2".into()),
            2,
            2,
            4,
            0,
            0,
            0,
            0.2,
            false,
        );
        ok_b.ts = "2026-07-12T00:00:00Z".into();
        ok_b.status = "ok".into();
        ok_b.ttfb_ms = Some(80);
        ok_b.duration_ms = Some(200);
        repo.insert(&ok_b).await.unwrap();

        let outcome = repo.summary_outcome("2026-07", None, 0).await.unwrap();
        assert_eq!(outcome.request_count, 3);
        assert_eq!(outcome.success_count, 2);
        assert!((outcome.success_rate - 2.0 / 3.0).abs() < 1e-9);
        let avg_ttfb = outcome.avg_ttfb_ms.unwrap();
        assert!((avg_ttfb - 60.0).abs() < 1e-6); // (40+80)/2
        // ok: 1 tok / (100-40)ms; ok_b: 2 tok / (200-80)ms; err has 0 tokens, excluded.
        // sum/sum = 3 tok / 180ms * 1000 = 16.666.. tok/s
        let tps = outcome.tokens_per_sec.unwrap();
        assert!((tps - 16.666_666_666_666_668).abs() < 1e-6);

        let by_p = repo.summary_by_provider("2026-07", None, 0).await.unwrap();
        let a = by_p.iter().find(|p| p.provider_id == "prov-a").unwrap();
        assert_eq!(a.request_count, 2);
        assert!((a.success_rate - 0.5).abs() < 1e-9);
        assert!((a.avg_ttfb_ms.unwrap() - 40.0).abs() < 1e-6);
        // Only the ok row is eligible (err has 0 completion_tokens): 1 tok / 60ms * 1000.
        assert!((a.tokens_per_sec.unwrap() - 16.666_666_666_666_668).abs() < 1e-6);
        let b = by_p.iter().find(|p| p.provider_id == "prov-b").unwrap();
        assert_eq!(b.request_count, 1);
        assert!((b.success_rate - 1.0).abs() < 1e-9);
        // 2 tok / (200-80)ms * 1000.
        assert!((b.tokens_per_sec.unwrap() - 16.666_666_666_666_668).abs() < 1e-6);
    }

    #[tokio::test]
    async fn tokens_per_sec_edge_cases() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        // Row with no duration_ms at all must not leak its completion_tokens
        // into the numerator (numerator/denominator filters must match).
        let mut no_duration = new_usage_record(
            "r-no-duration",
            Some("k1".into()),
            None,
            Some("prov-a".into()),
            Some("openai".into()),
            Some("m".into()),
            5,
            10,
            15,
            0,
            0,
            0,
            0.05,
            true,
        );
        no_duration.ts = "2026-07-01T00:00:00Z".into();
        no_duration.status = "ok".into();
        no_duration.duration_ms = None;
        no_duration.ttfb_ms = None;
        repo.insert(&no_duration).await.unwrap();

        // Dirty data: ttfb_ms > duration_ms must clamp generation time to 0,
        // contributing 0 to the denominator (not negative).
        let mut dirty = new_usage_record(
            "r-dirty",
            Some("k1".into()),
            None,
            Some("prov-a".into()),
            Some("openai".into()),
            Some("m".into()),
            1,
            20,
            21,
            0,
            0,
            0,
            0.01,
            true,
        );
        dirty.ts = "2026-07-02T00:00:00Z".into();
        dirty.status = "ok".into();
        dirty.duration_ms = Some(100);
        dirty.ttfb_ms = Some(150);
        repo.insert(&dirty).await.unwrap();

        // A normal eligible row so the denominator isn't 0 overall -- this
        // makes the no_duration-leak scenario distinguishable: if its 10
        // tokens wrongly leaked into the numerator, the rate would be
        // (10+20+6)*1000/100 = 360 instead of the correct (20+6)*1000/100 = 260.
        let mut valid = new_usage_record(
            "r-valid",
            Some("k1".into()),
            None,
            Some("prov-a".into()),
            Some("openai".into()),
            Some("m".into()),
            3,
            6,
            9,
            0,
            0,
            0,
            0.02,
            true,
        );
        valid.ts = "2026-07-03T00:00:00Z".into();
        valid.status = "ok".into();
        valid.duration_ms = Some(100);
        valid.ttfb_ms = Some(0);
        repo.insert(&valid).await.unwrap();

        let outcome = repo.summary_outcome("2026-07", None, 0).await.unwrap();
        // no_duration is excluded entirely (duration_ms IS NULL); dirty
        // contributes 20 tokens / 0ms (clamped); valid contributes 6 tokens /
        // 100ms. sum/sum = (20+6)*1000/(0+100) = 260 tok/s.
        let tps = outcome.tokens_per_sec.unwrap();
        assert!((tps - 260.0).abs() < 1e-6);

        let by_p = repo.summary_by_provider("2026-07", None, 0).await.unwrap();
        let a = by_p.iter().find(|p| p.provider_id == "prov-a").unwrap();
        assert!((a.tokens_per_sec.unwrap() - 260.0).abs() < 1e-6);

        // A group where every row has completion_tokens == 0 must yield None,
        // not 0.0 or a panic from division by zero.
        let pool2 = open_db("sqlite::memory:").await.unwrap();
        let repo2 = UsageRepo::new(&pool2);
        let mut all_error = new_usage_record(
            "r-all-error",
            Some("k1".into()),
            None,
            Some("prov-z".into()),
            Some("openai".into()),
            Some("m".into()),
            5,
            0,
            5,
            0,
            0,
            0,
            0.0,
            false,
        );
        all_error.ts = "2026-07-03T00:00:00Z".into();
        all_error.status = "error".into();
        all_error.duration_ms = Some(10);
        repo2.insert(&all_error).await.unwrap();

        let outcome2 = repo2.summary_outcome("2026-07", None, 0).await.unwrap();
        assert!(outcome2.tokens_per_sec.is_none());

        let by_p2 = repo2.summary_by_provider("2026-07", None, 0).await.unwrap();
        assert_eq!(by_p2.len(), 1);
        assert!(by_p2[0].tokens_per_sec.is_none());
    }

    #[tokio::test]
    async fn summary_by_model_tokens_per_sec() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = UsageRepo::new(&pool);

        let mut fast = new_usage_record(
            "r-fast",
            Some("k1".into()),
            Some("gpt-fast".into()),
            Some("prov-a".into()),
            Some("openai".into()),
            Some("gpt-fast".into()),
            1,
            100,
            101,
            0,
            0,
            0,
            0.1,
            true,
        );
        fast.ts = "2026-07-10T00:00:00Z".into();
        fast.status = "ok".into();
        fast.ttfb_ms = Some(0);
        fast.duration_ms = Some(500); // 100 tok / 500ms * 1000 = 200 tok/s
        repo.insert(&fast).await.unwrap();

        let mut slow = new_usage_record(
            "r-slow",
            Some("k1".into()),
            Some("gpt-slow".into()),
            Some("prov-a".into()),
            Some("openai".into()),
            Some("gpt-slow".into()),
            1,
            50,
            51,
            0,
            0,
            0,
            0.1,
            true,
        );
        slow.ts = "2026-07-10T00:00:00Z".into();
        slow.status = "ok".into();
        slow.ttfb_ms = Some(0);
        slow.duration_ms = Some(1000); // 50 tok / 1000ms * 1000 = 50 tok/s
        repo.insert(&slow).await.unwrap();

        let by_model = repo.summary_by_model("2026-07", None, 0).await.unwrap();
        let fast_row = by_model.iter().find(|m| m.label == "gpt-fast").unwrap();
        assert!((fast_row.tokens_per_sec.unwrap() - 200.0).abs() < 1e-6);
        let slow_row = by_model.iter().find(|m| m.label == "gpt-slow").unwrap();
        assert!((slow_row.tokens_per_sec.unwrap() - 50.0).abs() < 1e-6);
    }
}
