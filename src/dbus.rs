// SPDX-License-Identifier: GPL-3.0-only
//! Session-bus interface used to control the running daemon.
//!
//! `cosmic-quake-term --toggle` is a thin client that calls these methods, so the
//! shortcut can be bound in COSMIC Settings → Keyboard → Custom Shortcuts.

use tokio::sync::mpsc::UnboundedSender;

pub const NAME: &str = "dev.norman.CosmicQuakeTerm";
pub const PATH: &str = "/dev/norman/CosmicQuakeTerm";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Toggle,
    Show,
    Hide,
    Quit,
    NewTab,
    SplitHorizontal,
    SplitVertical,
}

impl Command {
    pub fn method(self) -> &'static str {
        match self {
            Command::Toggle => "Toggle",
            Command::Show => "Show",
            Command::Hide => "Hide",
            Command::Quit => "Quit",
            Command::NewTab => "NewTab",
            Command::SplitHorizontal => "SplitHorizontal",
            Command::SplitVertical => "SplitVertical",
        }
    }
}

/// Names of the connected outputs, kept up to date by the daemon for `--list-monitors`.
pub static MONITOR_NAMES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

struct Service(UnboundedSender<Command>);

#[zbus::interface(name = "dev.norman.CosmicQuakeTerm")]
impl Service {
    fn toggle(&self) {
        let _ = self.0.send(Command::Toggle);
    }

    fn show(&self) {
        let _ = self.0.send(Command::Show);
    }

    fn hide(&self) {
        let _ = self.0.send(Command::Hide);
    }

    fn quit(&self) {
        let _ = self.0.send(Command::Quit);
    }

    fn new_tab(&self) {
        let _ = self.0.send(Command::NewTab);
    }

    fn split_horizontal(&self) {
        let _ = self.0.send(Command::SplitHorizontal);
    }

    fn split_vertical(&self) {
        let _ = self.0.send(Command::SplitVertical);
    }

    fn list_monitors(&self) -> Vec<String> {
        MONITOR_NAMES
            .lock()
            .map(|names| names.clone())
            .unwrap_or_default()
    }
}

/// Claim the well-known name and export the control object.
/// The returned connection must be kept alive for as long as the service should run.
pub async fn serve(tx: UnboundedSender<Command>) -> zbus::Result<zbus::Connection> {
    zbus::connection::Builder::session()?
        .name(NAME)?
        .serve_at(PATH, Service(tx))?
        .build()
        .await
}

/// Is a daemon already running (does anyone own our bus name)?
pub fn is_running() -> bool {
    let Ok(conn) = zbus::blocking::Connection::session() else {
        return false;
    };
    conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "NameHasOwner",
        &(NAME,),
    )
    .ok()
    .and_then(|reply| reply.body().deserialize::<bool>().ok())
    .unwrap_or(false)
}

/// Send a command to the running daemon.
pub fn send(command: Command) -> zbus::Result<()> {
    let conn = zbus::blocking::Connection::session()?;
    conn.call_method(Some(NAME), PATH, Some(NAME), command.method(), &())?;
    Ok(())
}

/// Ask the running daemon which outputs it sees.
pub fn list_monitors() -> zbus::Result<Vec<String>> {
    let conn = zbus::blocking::Connection::session()?;
    let reply = conn.call_method(Some(NAME), PATH, Some(NAME), "ListMonitors", &())?;
    reply.body().deserialize::<Vec<String>>()
}
