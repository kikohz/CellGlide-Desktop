//! One module per sidebar page. A page is plain state: `update` returns an `Effect` instead of
//! doing I/O, and `view` draws the state. Overview only shows the app's state and sends app
//! messages, so it has no state of its own.

pub mod about;
pub mod diagnostics;
pub mod history;
pub mod leak;
pub mod overview;
pub mod settings;
pub mod speed;

use crate::effect::Effect;
use crate::prefs::Prefs;
use cg_ipc::describe::ui::Section;
use cg_ipc::{ConnState, DnsMode, Snapshot};
use iced::Element;
use jiff::Timestamp;

/// What every page may read from the app.
pub struct Ctx<'a> {
    pub snapshot: Option<&'a Snapshot>,
    pub now: Timestamp,
    pub prefs: &'a Prefs,
}

impl Ctx<'_> {
    /// Connected through a phone whose card the service publishes.
    pub fn phone_connected(&self) -> bool {
        self.snapshot
            .is_some_and(|s| matches!(s.state, ConnState::Connected { .. }) && s.phone.is_some())
    }

    /// How DNS is handled while connected; `None` otherwise.
    pub fn dns(&self) -> Option<DnsMode> {
        match self.snapshot.map(|s| &s.state) {
            Some(ConnState::Connected { dns, .. }) => Some(*dns),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PageMsg {
    Speed(speed::Msg),
    Leak(leak::Msg),
    History(history::Msg),
    Diagnostics(diagnostics::Msg),
    Settings(settings::Msg),
    About(about::Msg),
}

#[derive(Debug, Default)]
pub struct Pages {
    pub speed: speed::State,
    pub leak: leak::State,
    pub history: history::State,
    pub diagnostics: diagnostics::State,
    pub settings: settings::State,
    pub about: about::State,
}

impl Pages {
    pub fn update(&mut self, message: PageMsg, ctx: &Ctx) -> Effect {
        match message {
            PageMsg::Speed(m) => speed::update(&mut self.speed, m, ctx),
            PageMsg::Leak(m) => leak::update(&mut self.leak, m, ctx),
            PageMsg::History(m) => history::update(&mut self.history, m, ctx),
            PageMsg::Diagnostics(m) => diagnostics::update(&mut self.diagnostics, m, ctx),
            PageMsg::Settings(m) => settings::update(&mut self.settings, m, ctx),
            PageMsg::About(m) => about::update(&mut self.about, m, ctx),
        }
    }

    /// Any page but Overview.
    pub fn view<'a>(&'a self, section: Section, ctx: &Ctx<'a>) -> Element<'a, PageMsg> {
        match section {
            Section::SpeedTest => speed::view(&self.speed, ctx).map(PageMsg::Speed),
            Section::DnsLeak => leak::view(&self.leak, ctx).map(PageMsg::Leak),
            Section::History => history::view(&self.history, ctx).map(PageMsg::History),
            Section::Diagnostics => {
                diagnostics::view(&self.diagnostics, ctx).map(PageMsg::Diagnostics)
            }
            Section::Settings => settings::view(&self.settings, ctx).map(PageMsg::Settings),
            Section::About => about::view(&self.about, ctx).map(PageMsg::About),
            // The app draws Overview itself (`overview::view`).
            Section::Overview => iced::widget::column![].into(),
        }
    }
}

#[cfg(test)]
impl Ctx<'static> {
    /// A context for page tests; the snapshot and default preferences are leaked, which is
    /// fine in a test process.
    pub fn for_test(snapshot: Option<Snapshot>) -> Ctx<'static> {
        Ctx {
            snapshot: snapshot.map(|s| &*Box::leak(Box::new(s))),
            now: Timestamp::now(),
            prefs: Box::leak(Box::new(Prefs::default())),
        }
    }
}

/// Snapshots for page tests.
#[cfg(test)]
pub mod fixtures {
    use cg_ipc::phone::{PhoneCard, PhoneFrame, PhoneLabel};
    use cg_ipc::{ConnState, DnsMode, Snapshot};

    pub const SERIAL: &str = "00008140-000000000000002C";

    /// Connected through one iPhone that reports a frame: "phone connected" for the pages.
    pub fn connected_snapshot() -> Snapshot {
        let mut snapshot = disconnected_snapshot();
        snapshot.state = ConnState::Connected {
            phone_version: "2.4.0".into(),
            dns: DnsMode::Phone,
        };
        snapshot.wants_connection = true;
        snapshot.phone = Some(PhoneCard {
            frame: Some(PhoneFrame {
                carrier: Some("中国移动".into()),
                radio: Some("5G".into()),
                sharing: Some(true),
                download: Some(2_500_000.0),
                upload: Some(400_000.0),
                quality_score: Some(90),
                latency: Some(38.0),
                ..PhoneFrame::default()
            }),
            ..PhoneCard::default()
        });
        snapshot.socks_port = Some(1080);
        snapshot
    }

    /// The same iPhone plugged in, nobody asked to connect.
    pub fn disconnected_snapshot() -> Snapshot {
        Snapshot {
            state: ConnState::Idle,
            devices: vec![SERIAL.into()],
            selected: Some(SERIAL.into()),
            labels: [(
                SERIAL.to_string(),
                PhoneLabel {
                    name: Some("Test iPhone".into()),
                    model: Some("iPhone17,5".into()),
                },
            )]
            .into(),
            ..Snapshot::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn phone_connected_needs_both_the_state_and_a_card() {
        assert!(Ctx::for_test(Some(connected_snapshot())).phone_connected());
        assert!(!Ctx::for_test(Some(disconnected_snapshot())).phone_connected());
        assert!(!Ctx::for_test(None).phone_connected());
        let mut no_card = connected_snapshot();
        no_card.phone = None;
        assert!(!Ctx::for_test(Some(no_card)).phone_connected());
    }

    #[test]
    fn dns_only_while_connected() {
        assert_eq!(
            Ctx::for_test(Some(connected_snapshot())).dns(),
            Some(DnsMode::Phone)
        );
        assert_eq!(Ctx::for_test(Some(disconnected_snapshot())).dns(), None);
        assert_eq!(Ctx::for_test(None).dns(), None);
    }
}
