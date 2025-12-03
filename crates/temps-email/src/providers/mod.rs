//! Email provider abstractions and implementations

mod scaleway;
mod ses;
mod traits;

#[cfg(test)]
pub mod mock;

pub use scaleway::{ScalewayCredentials, ScalewayProvider};
pub use ses::{SesCredentials, SesProvider};
pub use traits::*;

#[cfg(test)]
pub use mock::MockEmailProvider;
