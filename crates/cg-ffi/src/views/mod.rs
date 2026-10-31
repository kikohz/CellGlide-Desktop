//! What the pages bind to: text already translated, colours only as a meaning. Every field is
//! snake_case, an `Option` is `null`, and there are no tuples or fixed-size arrays (named
//! structs and `Vec` instead), so C# can read them with plain records. `ui::` types are reused
//! as they are; where one of them is a tuple or a fixed array (`menu_layout`, `phone_options`,
//! `GROUPS`, `AutoConnectSwitch`), the view carries a named shape instead.
//!
//! Everything here is a pure function of its arguments: the time and the time zone come from
//! the caller.

mod chrome;
mod history;
mod overview;

pub use chrome::{
    BadgesView, ChromeView, MenuItemView, MenuKind, SidebarGroupView, SidebarItemView, chrome,
};
pub use history::{DetailItem, HistoryRowView, HistoryView, TickView, checks, history};
pub use overview::{
    BatteryView, HealthRowView, HeatView, OverviewView, PhoneCardView, SettingsView, overview,
};

use serde::{Deserialize, Serialize};

/// The speed test page while a test runs and after it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SpeedView {}

/// The DNS leak test page.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct LeakView {}
