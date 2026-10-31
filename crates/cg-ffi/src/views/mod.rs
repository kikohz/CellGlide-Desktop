//! What the pages bind to: text already translated, colours only as a meaning. Every field is
//! snake_case, an `Option` is `null`, and there are no tuples or fixed-size arrays (named
//! structs and `Vec` instead), so C# can read them with plain records.
//!
//! The views are empty for now; Tasks 9 and 10 fill them.

use serde::{Deserialize, Serialize};

/// The overview page, the tray and the panel, from the service's snapshot.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct OverviewView {}

/// The speed test page while a test runs and after it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SpeedView {}

/// The DNS leak test page.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct LeakView {}

/// One day of the connection history.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct HistoryView {}
