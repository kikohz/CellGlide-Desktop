//! The diagnostics bundle a user can send us: its format, the redactor that scrubs it,
//! the HTTPS upload, and saving it to a file. Same format as the Mac app's, with platform
//! "linux".

pub mod bundle;
pub mod redact;
pub mod save;
pub mod upload;
