//! The main window's sidebar: sections in the Mac's order, their groups, and the dots beside them.

use crate::tray::ServiceDown;
use cg_ipc::{ConnState, DnsMode, Snapshot};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Section {
    #[default]
    Overview,
    SpeedTest,
    DnsLeak,
    History,
    Diagnostics,
    Settings,
    About,
}

impl Section {
    #[cfg(test)]
    pub const ALL: [Section; 7] = [
        Section::Overview,
        Section::SpeedTest,
        Section::DnsLeak,
        Section::History,
        Section::Diagnostics,
        Section::Settings,
        Section::About,
    ];

    pub fn title_key(self) -> &'static str {
        match self {
            Section::Overview => "nav.overview",
            Section::SpeedTest => "nav.speed_test",
            Section::DnsLeak => "nav.dns_leak",
            Section::History => "nav.history",
            Section::Diagnostics => "nav.diagnostics",
            Section::Settings => "nav.settings",
            Section::About => "nav.about",
        }
    }
}

/// The sidebar's groups, each with an optional heading key, as on the Mac (no map or split).
pub const GROUPS: [(Option<&str>, &[Section]); 4] = [
    (None, &[Section::Overview]),
    (Some("nav.tools"), &[Section::SpeedTest, Section::DnsLeak]),
    (
        Some("nav.activity"),
        &[Section::History, Section::Diagnostics],
    ),
    (Some("nav.app"), &[Section::Settings, Section::About]),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    None,
    Warning,
    Critical,
}

/// The dot beside a section, after the Mac's `MainSectionBadge`. Overview: an open issue (red
/// when severe), else a connection waiting on the user; starting up is transient and not marked.
/// History: an open issue, same colour. Settings: the user can't talk to the service for lack
/// of the group, or DNS couldn't be set (the Mac's missing background permission). Nothing else.
pub fn badge(
    section: Section,
    snapshot: Option<&Snapshot>,
    service: Option<&ServiceDown>,
) -> Badge {
    let issue = snapshot.and_then(|s| s.health.cause).map(|cause| {
        if cause.is_severe() {
            Badge::Critical
        } else {
            Badge::Warning
        }
    });
    match section {
        Section::Overview => issue.unwrap_or_else(|| match snapshot.map(|s| &s.state) {
            Some(ConnState::Retrying { .. } | ConnState::ChoosePhone) => Badge::Warning,
            _ => Badge::None,
        }),
        Section::History => issue.unwrap_or(Badge::None),
        Section::Settings => {
            let outside = service.is_some_and(|s| s.not_in_group);
            let unmanaged = snapshot.is_some_and(|s| {
                matches!(
                    s.state,
                    ConnState::Connected {
                        dns: DnsMode::Unmanaged,
                        ..
                    }
                )
            });
            if outside || unmanaged {
                Badge::Warning
            } else {
                Badge::None
            }
        }
        Section::SpeedTest | Section::DnsLeak | Section::Diagnostics | Section::About => {
            Badge::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_ipc::health::{HealthStatus, IssueCause};
    use cg_ipc::{ConnState, DnsMode, Reason, Snapshot};

    #[test]
    fn sidebar_order_and_groups() {
        let flat: Vec<Section> = GROUPS.iter().flat_map(|(_, s)| s.iter().copied()).collect();
        assert_eq!(flat, Section::ALL.to_vec());
    }

    fn with(state: ConnState, cause: Option<IssueCause>) -> Snapshot {
        Snapshot {
            state,
            health: HealthStatus {
                cause,
                ..HealthStatus::default()
            },
            ..Snapshot::default()
        }
    }

    fn connected(dns: DnsMode) -> ConnState {
        ConnState::Connected {
            phone_version: "2.4.0".into(),
            dns,
        }
    }

    #[test]
    fn badges_follow_the_mac_rules() {
        let severe = with(connected(DnsMode::Phone), Some(IssueCause::PhoneNetwork));
        let mild = with(connected(DnsMode::Phone), Some(IssueCause::PhoneUnstable));
        let retrying = with(
            ConnState::Retrying {
                reason: Reason::NoExit,
            },
            None,
        );
        let choose = with(ConnState::ChoosePhone, None);
        let connecting = with(ConnState::Connecting, None);
        let unmanaged = with(connected(DnsMode::Unmanaged), None);
        let fine = with(connected(DnsMode::Phone), None);
        let outside = ServiceDown {
            text: "x".into(),
            not_in_group: true,
        };
        let stopped = ServiceDown {
            text: "x".into(),
            not_in_group: false,
        };

        assert_eq!(
            badge(Section::Overview, Some(&severe), None),
            Badge::Critical
        );
        assert_eq!(
            badge(Section::History, Some(&severe), None),
            Badge::Critical
        );
        assert_eq!(badge(Section::Overview, Some(&mild), None), Badge::Warning);
        assert_eq!(badge(Section::History, Some(&mild), None), Badge::Warning);
        assert_eq!(
            badge(Section::Overview, Some(&retrying), None),
            Badge::Warning
        );
        assert_eq!(badge(Section::History, Some(&retrying), None), Badge::None);
        assert_eq!(
            badge(Section::Overview, Some(&choose), None),
            Badge::Warning
        );
        assert_eq!(
            badge(Section::Overview, Some(&connecting), None),
            Badge::None
        );
        assert_eq!(
            badge(Section::History, Some(&connecting), None),
            Badge::None
        );
        assert_eq!(badge(Section::Overview, Some(&fine), None), Badge::None);
        assert_eq!(
            badge(Section::Settings, Some(&unmanaged), None),
            Badge::Warning
        );
        assert_eq!(badge(Section::Settings, Some(&fine), None), Badge::None);
        assert_eq!(
            badge(Section::Settings, None, Some(&outside)),
            Badge::Warning
        );
        assert_eq!(badge(Section::Settings, None, Some(&stopped)), Badge::None);
        assert_eq!(badge(Section::Overview, None, Some(&stopped)), Badge::None);

        let cases = [
            (Some(&severe), None),
            (Some(&mild), None),
            (Some(&retrying), None),
            (Some(&choose), None),
            (Some(&connecting), None),
            (Some(&unmanaged), None),
            (None, Some(&outside)),
        ];
        for (snapshot, service) in cases {
            for section in [
                Section::About,
                Section::SpeedTest,
                Section::DnsLeak,
                Section::Diagnostics,
            ] {
                assert_eq!(
                    badge(section, snapshot, service),
                    Badge::None,
                    "{section:?}"
                );
            }
        }
    }

    #[test]
    fn every_section_has_a_title_key() {
        for section in Section::ALL {
            assert!(section.title_key().starts_with("nav."));
        }
    }
}
