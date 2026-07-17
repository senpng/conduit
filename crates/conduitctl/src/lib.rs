//! Library surface for `conduitctl` — shared admin client, DTOs, utilities.

pub mod admin_client;
pub mod dto;
pub mod util;

pub use admin_client::{provider_create_request_body, route_admin_path, AdminClient, AdminError};
pub use dto::{
    CreateKeyBody, CreateProviderBody, CreateRouteBody, HealthResponse, KeyCreateResponse,
    TraceIndexRowDto, TraceListResponse,
};
pub use util::sse::{classify_sse_frame, extract_sse_data, parse_sse_frame, SseFrame};
