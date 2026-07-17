//! L1-L7 pipeline orchestration.
//!
//! The pipeline is a strict layered stack:
//!   L1 Transport      — axum/tower handles this upstream of pipeline
//!   L2 Ingress Filter — auth, quota checks (see ingress.rs)
//!   L3 Router         — pure function routing decision (see stage.rs)
//!   L4 Codec          — IR ↔ wire translation (conduit-codec)
//!   L5 Upstream       — provider HTTP call (conduit-upstream)
//!   L6 Egress Filter  — finalize, compute cost, emit trace events
//!   L7 Sink           — event bus → trace store

pub mod context;
pub mod egress;
pub mod handle;
pub mod ingress;
pub mod provider;
pub mod stage;
pub mod stream_probe;

pub use context::{IngressWire, PipelineContext};
pub use handle::{
    AuthFn, BoxFut, KeyPolicyFn, PipelineDeps, PipelineHandle, PipelineResult, PricingFn,
};
pub use ingress::KeyPolicy;
pub use provider::{ProviderKind, UpstreamAuth};
