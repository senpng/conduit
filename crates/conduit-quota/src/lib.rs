//! conduit-quota — Rate-limit enforcement and usage recording hooks.
//!
//! This crate provides pure in-memory quota logic (RPM sliding-window counter)
//! that is decoupled from database I/O. Callers wire in their own usage ledger
//! via a `record_fn` closure. Budget *limits* are intentionally not enforced
//! here; spend is only recorded.
//!
//! # Usage
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use conduit_quota::{InMemoryQuotaEngine, QuotaEngine, QuotaCheckRequest};
//!
//! let engine = InMemoryQuotaEngine::new(
//!     Arc::new(|req| {
//!         Box::pin(async move {
//!             // Persist req into usage_records …
//!             Ok(())
//!         })
//!     }),
//! );
//! ```

pub mod bucket;
pub mod check;
pub mod engine;

// Flatten the most-used types into the crate root.
pub use bucket::SlidingWindowCounter;
pub use check::{
    BoxFuture, QuotaAttemptRecord, QuotaCheckRequest, QuotaRecordRequest, RecordFn,
};
// Re-export the canonical QuotaError from conduit-ir.
pub use conduit_ir::error::QuotaError;
pub use engine::{InMemoryQuotaEngine, NoopQuotaEngine, QuotaEngine};
