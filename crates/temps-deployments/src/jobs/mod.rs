//! Concrete job implementations for deployment workflows
//!
//! This module provides ready-to-use job implementations for common deployment tasks.

pub mod build_image;
pub mod configure_crons;
pub mod deploy_image;
pub mod deploy_static;
pub mod deploy_static_bundle;
pub mod download_repo;
pub mod mark_deployment_complete;
pub mod pipeline_validation;
pub mod pull_external_image;
pub mod scan_vulnerabilities;
pub mod take_screenshot;

pub use build_image::*;
pub use configure_crons::*;
pub use deploy_image::*;
pub use deploy_static::*;
pub use deploy_static_bundle::*;
pub use download_repo::*;
pub use mark_deployment_complete::*;
pub use pull_external_image::*;
pub use scan_vulnerabilities::*;
pub use take_screenshot::*;
