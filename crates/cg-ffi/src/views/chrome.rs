//! What the window and the tray show around the pages, with a snapshot or without one: the
//! tray's look and menu, the sidebar and its dots. `State` carries it inside the overview and
//! `ServiceGone` carries it on its own, so the app never works any of it out itself.

use cg_i18n::tr;
use cg_ipc::Snapshot;
use cg_ipc::describe::ui::{self, ServiceDown};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChromeView {
    pub tray: ui::TrayLook,
    /// The tray's right-click menu, in order.
    pub menu: Vec<MenuItemView>,
    pub badges: BadgesView,
    /// The sidebar's groups in the Mac's order; the dots are in `badges`.
    pub sidebar: Vec<SidebarGroupView>,
}

/// The dots beside the sections that can have one.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BadgesView {
    pub overview: ui::Badge,
    pub history: ui::Badge,
    pub settings: ui::Badge,
}

/// One entry of the tray's menu (`ui::menu_layout`, which is a list of pairs).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MenuItemView {
    pub kind: MenuKind,
    /// Empty for a separator.
    pub label: String,
    pub enabled: bool,
}

/// What a menu entry does. `toggle` sends `Command::Toggle`; its label says which way.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MenuKind {
    Version,
    Separator,
    Toggle,
    Open,
    Quit,
}

/// One group of the sidebar (`ui::GROUPS`, which is a fixed array of pairs).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SidebarGroupView {
    pub heading: Option<String>,
    pub sections: Vec<SidebarItemView>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SidebarItemView {
    pub section: ui::Section,
    pub title: String,
}

/// The chrome for the last snapshot, or while there is none: `down` says why (the tray then
/// shows the reason, and Settings a dot when the user isn't in the group). Neither: waiting for
/// the service.
pub fn chrome(snapshot: Option<&Snapshot>, down: Option<&ServiceDown>) -> ChromeView {
    let tray = match (snapshot, down) {
        (None, Some(down)) => ui::down_look(down),
        _ => ui::tray_look(snapshot),
    };
    let badge = |section| ui::badge(section, snapshot, down);
    ChromeView {
        menu: menu(&tray),
        tray,
        badges: BadgesView {
            overview: badge(ui::Section::Overview),
            history: badge(ui::Section::History),
            settings: badge(ui::Section::Settings),
        },
        sidebar: sidebar(),
    }
}

fn menu(look: &ui::TrayLook) -> Vec<MenuItemView> {
    ui::menu_layout(look)
        .into_iter()
        .map(|(entry, enabled)| {
            let (kind, label) = match entry {
                ui::MenuEntry::Version => (MenuKind::Version, ui::version_line()),
                ui::MenuEntry::Separator => (MenuKind::Separator, String::new()),
                ui::MenuEntry::Toggle { disconnect } => {
                    let key = if disconnect {
                        "tray.disconnect"
                    } else {
                        "tray.connect"
                    };
                    (MenuKind::Toggle, tr(key, &[]))
                }
                ui::MenuEntry::Open => (MenuKind::Open, tr("tray.open", &[])),
                ui::MenuEntry::Quit => (MenuKind::Quit, tr("tray.quit", &[])),
            };
            MenuItemView {
                kind,
                label,
                enabled,
            }
        })
        .collect()
}

fn sidebar() -> Vec<SidebarGroupView> {
    ui::GROUPS
        .iter()
        .map(|(heading, sections)| SidebarGroupView {
            heading: heading.map(|key| tr(key, &[])),
            sections: sections
                .iter()
                .map(|&section| SidebarItemView {
                    section,
                    title: tr(section.title_key(), &[]),
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{connected_snapshot, disconnected_snapshot};
    use cg_ipc::describe::ui::{Badge, DownReason, Section};

    fn en() -> std::sync::MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::En);
        guard
    }

    #[test]
    fn the_menu_and_the_sidebar_are_named_lists() {
        let _g = en();
        let c = chrome(Some(&connected_snapshot()), None);
        let kinds: Vec<MenuKind> = c.menu.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            [
                MenuKind::Version,
                MenuKind::Separator,
                MenuKind::Toggle,
                MenuKind::Open,
                MenuKind::Separator,
                MenuKind::Quit
            ]
        );
        assert!(!c.menu[0].enabled && c.menu[0].label == ui::version_line());
        assert_eq!(c.menu[2].label, tr("tray.disconnect", &[]));
        let idle = chrome(Some(&disconnected_snapshot()), None);
        assert_eq!(idle.menu[2].label, tr("tray.connect", &[]));
        let order: Vec<Section> = c
            .sidebar
            .iter()
            .flat_map(|g| g.sections.iter().map(|s| s.section))
            .collect();
        assert_eq!(order, Section::ALL);
        assert_eq!(c.sidebar[0].heading, None);
        assert_eq!(
            c.sidebar[1].heading.as_deref(),
            Some(tr("nav.tools", &[]).as_str())
        );
        assert_eq!(c.sidebar[0].sections[0].title, tr("nav.overview", &[]));
    }

    #[test]
    fn without_a_snapshot_the_tray_says_why_and_offers_to_connect() {
        let _g = en();
        let outside = ServiceDown::new(DownReason::NotInGroup);
        let c = chrome(None, Some(&outside));
        assert_eq!(c.tray, ui::down_look(&outside));
        assert!(!c.tray.active && !c.tray.wants);
        assert_eq!(c.menu[2].kind, MenuKind::Toggle);
        assert_eq!(c.menu[2].label, tr("tray.connect", &[]));
        assert_eq!(c.badges.settings, Badge::Warning);
        assert_eq!(c.badges.overview, Badge::None);
        assert_eq!(c.sidebar, chrome(Some(&connected_snapshot()), None).sidebar);
        let stopped = chrome(None, Some(&ServiceDown::new(DownReason::Gone)));
        assert_eq!(stopped.badges.settings, Badge::None);
        assert_eq!(stopped.tray.title, tr("gui.service_gone", &[]));
        // Nothing heard yet: waiting for the service.
        let waiting = chrome(None, None);
        assert_eq!(waiting.tray.title, tr("gui.waiting_service", &[]));
        assert_eq!(crate::testing::plain_shape(&c), Ok(()));
    }
}
