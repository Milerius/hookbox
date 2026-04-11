//! Provider signature verification adapters for hookbox.

#[cfg(feature = "generic-hmac")]
pub mod generic_hmac;
#[cfg(feature = "stripe")]
pub mod stripe;
#[cfg(feature = "adyen")]
pub mod adyen;
#[cfg(feature = "bvnk")]
pub mod bvnk;
#[cfg(feature = "triplea")]
pub mod triplea;
#[cfg(feature = "triplea")]
pub mod triplea_crypto;
#[cfg(feature = "walapay")]
pub mod walapay;

#[cfg(feature = "generic-hmac")]
pub use generic_hmac::GenericHmacVerifier;
#[cfg(feature = "stripe")]
pub use stripe::StripeVerifier;
#[cfg(feature = "adyen")]
pub use adyen::AdyenVerifier;
#[cfg(feature = "bvnk")]
pub use bvnk::BvnkVerifier;
#[cfg(feature = "triplea")]
pub use triplea::TripleAFiatVerifier;
#[cfg(feature = "triplea")]
pub use triplea_crypto::TripleACryptoVerifier;
#[cfg(feature = "walapay")]
pub use walapay::WalapayVerifier;
