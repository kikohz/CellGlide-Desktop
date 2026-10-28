//! Tells the controller when the computer suspends and resumes, from logind's
//! `PrepareForSleep` signal, so the health judge doesn't read the gap as an outage. Only
//! listens: no logind method is called, no inhibitor lock is taken.

use cg_core::controller::{Command, Controller};
use std::future::poll_fn;
use std::pin::Pin;
use zbus::export::futures_core::Stream;

/// Runs until the signal stream ends. A missing system bus or logind is logged once and
/// the function returns; the service carries on without sleep handling.
pub async fn watch(controller: Controller) {
    if let Err(error) = listen(&controller).await {
        tracing::warn!("not watching for suspend and resume: {error}");
    }
}

async fn listen(controller: &Controller) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let mut signals = proxy.receive_signal("PrepareForSleep").await?;
    while let Some(message) = poll_fn(|cx| Pin::new(&mut signals).poll_next(cx)).await {
        match message.body().deserialize::<bool>() {
            Ok(sleeping) => {
                if controller.send(Command::Sleep { sleeping }).await.is_err() {
                    // The controller is shutting down.
                    return Ok(());
                }
            }
            Err(error) => tracing::debug!("unreadable PrepareForSleep: {error}"),
        }
    }
    tracing::warn!("the logind connection closed; no longer watching for suspend and resume");
    Ok(())
}
