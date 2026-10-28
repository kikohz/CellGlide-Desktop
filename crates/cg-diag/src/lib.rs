//! The diagnostics bundle a user can send us: its format, the redactor that scrubs it,
//! and the HTTPS upload. Same format as the Mac app's, with platform "linux".

pub mod bundle;
pub mod redact;
pub mod upload;
