//! Core OTel services.

pub mod health_service;
pub mod otel_service;

pub use health_service::HealthComputeService;
pub use otel_service::OtelService;
