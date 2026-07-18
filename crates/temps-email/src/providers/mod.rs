//! Email provider abstractions and implementations

mod scaleway;
mod ses;
mod smtp;
mod traits;

#[cfg(test)]
pub mod mock;

pub use scaleway::{ScalewayCredentials, ScalewayProvider};
pub(crate) use ses::DEFAULT_CONFIGURATION_SET;
pub use ses::{SesCredentials, SesProvider};
pub use smtp::{SmtpCredentials, SmtpEncryption, SmtpProvider};
pub use traits::*;

#[cfg(test)]
pub use mock::MockEmailProvider;
