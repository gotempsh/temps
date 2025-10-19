//! Temps Import Orchestrator
//!
//! This crate provides the HTTP API and orchestration layer for importing workloads
//! into Temps from various sources (Docker, Coolify, Vercel, etc.).
//!
//! # Architecture
//!
//! - **Handlers**: HTTP endpoints for import operations
//! - **Services**: Business logic and orchestration
//! - **Plugin**: Integration with Temps plugin system
//!
//! # Usage
//!
//! This crate is registered as a plugin in the main Temps application.

pub mod handlers;
pub mod plugin;
pub mod services;

pub use plugin::ImportPlugin;
pub use services::ImportOrchestrator;
