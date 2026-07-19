//! Library surface for `conduitctl` — shared console client, DTOs, utilities.

pub mod console_client;
pub mod dto;
pub mod util;

pub use console_client::{
    provider_create_request_body, route_console_path, ConsoleClient, ConsoleError,
};
pub use dto::{
    CreateKeyBody, CreateProviderBody, CreateRouteBody, HealthResponse, KeyCreateResponse,
};
