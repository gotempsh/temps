pub(crate) mod domain_handler;	
pub(crate) mod types;

pub use domain_handler::{configure_routes};
pub use types::{create_domain_app_state, DomainAppState};