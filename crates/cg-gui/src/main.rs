#[cfg(unix)]
mod app;
#[cfg(unix)]
mod autostart;
#[cfg(unix)]
mod effect;
#[cfg(unix)]
mod ipc;
#[cfg(unix)]
mod pages;
#[cfg(unix)]
mod panel;
#[cfg(unix)]
mod prefs;
#[cfg(unix)]
mod single;
#[cfg(unix)]
mod tray;
#[cfg(unix)]
mod widgets;
#[cfg(unix)]
mod writer;

#[cfg(unix)]
fn main() -> iced::Result {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("{}", cg_ipc::describe::ui::version_line());
        return Ok(());
    }
    cg_i18n::init();
    // Another instance already runs in this session: it opens its window, this one exits.
    #[cfg(target_os = "linux")]
    let Some(_instance) = single::start(app::background_launch()) else {
        return Ok(());
    };
    iced::daemon(app::App::boot, app::App::update, app::App::view)
        .title(app::App::title)
        .subscription(app::App::subscription)
        .run()
}

#[cfg(not(unix))]
fn main() {
    eprintln!("cellglide-gui only runs on Linux");
}
