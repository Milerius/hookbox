//! Provider signature verification adapters for hookbox.

#[cfg(feature = "generic-hmac")]
pub mod generic_hmac;
#[cfg(feature = "stripe")]
pub mod stripe;

#[cfg(feature = "generic-hmac")]
pub use generic_hmac::GenericHmacVerifier;
#[cfg(feature = "stripe")]
pub use stripe::StripeVerifier;
