//! One GUI process per desktop session. The first GUI owns the D-Bus name
//! `io.github.oreo1298.Fossick`; starting another one, or `fossick --toggle`, forwards a command
//! to it instead (show or hide the window, open a new one, search for something).
//!
//! Commands carry the caller’s activation token (`XDG_ACTIVATION_TOKEN`), so Wayland
//! compositors let the window take focus.

use std::sync::Arc;

use zbus::blocking::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

pub const BUS_NAME: &str = "io.github.oreo1298.Fossick";
pub const OBJECT_PATH: &str = "/io/github/oreo1298/Fossick";
pub const INTERFACE: &str = "io.github.oreo1298.Fossick1";

/// What another process asks the running GUI to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Show the window if hidden or in the background, hide it if it is active.
    Toggle {
        token: String,
    },
    /// Show and focus the window.
    Activate {
        token: String,
    },
    NewWindow {
        token: String,
    },
    /// Show the window and search for `text` in a new tab.
    Search {
        text: String,
        token: String,
    },
}

impl Command {
    /// The caller’s activation token, from the environment.
    #[must_use]
    pub fn token_from_env() -> String {
        std::env::var("XDG_ACTIVATION_TOKEN")
            .or_else(|_| std::env::var("DESKTOP_STARTUP_ID"))
            .unwrap_or_default()
    }
}

type Handler = Arc<dyn Fn(Command) + Send + Sync>;

struct Service {
    handler: Handler,
}

#[zbus::interface(name = "io.github.oreo1298.Fossick1")]
impl Service {
    fn toggle(&self, token: &str) {
        (self.handler)(Command::Toggle {
            token: token.into(),
        });
    }

    fn activate(&self, token: &str) {
        (self.handler)(Command::Activate {
            token: token.into(),
        });
    }

    fn new_window(&self, token: &str) {
        (self.handler)(Command::NewWindow {
            token: token.into(),
        });
    }

    fn search(&self, text: &str, token: &str) {
        (self.handler)(Command::Search {
            text: text.into(),
            token: token.into(),
        });
    }
}

/// Keeps the D-Bus name while alive.
pub struct Instance {
    _conn: Connection,
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Instance")
    }
}

/// Whether this process became the GUI instance.
#[derive(Debug)]
pub enum Claim {
    /// This is the GUI; commands from others arrive through the handler.
    Primary(Instance),
    /// Another GUI runs and got `first`.
    Forwarded,
}

fn call(conn: &Connection, cmd: &Command) -> zbus::Result<()> {
    let target = (Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE));
    match cmd {
        Command::Toggle { token } => {
            conn.call_method(target.0, target.1, target.2, "Toggle", &(token,))?
        }
        Command::Activate { token } => {
            conn.call_method(target.0, target.1, target.2, "Activate", &(token,))?
        }
        Command::NewWindow { token } => {
            conn.call_method(target.0, target.1, target.2, "NewWindow", &(token,))?
        }
        Command::Search { text, token } => {
            conn.call_method(target.0, target.1, target.2, "Search", &(text, token))?
        }
    };
    Ok(())
}

/// Becomes the GUI instance (serving commands through `handler`, called on a D-Bus thread), or
/// forwards `first` to the GUI that already runs. Uses the session bus.
pub fn claim(
    first: &Command,
    handler: impl Fn(Command) + Send + Sync + 'static,
) -> zbus::Result<Claim> {
    claim_on(Connection::session()?, first, handler)
}

/// [`claim`] on a given bus connection.
pub fn claim_on(
    conn: Connection,
    first: &Command,
    handler: impl Fn(Command) + Send + Sync + 'static,
) -> zbus::Result<Claim> {
    // Serve before owning the name, so no caller finds the name without the object.
    conn.object_server().at(
        OBJECT_PATH,
        Service {
            handler: Arc::new(handler),
        },
    )?;
    match conn.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            Ok(Claim::Primary(Instance { _conn: conn }))
        }
        Ok(RequestNameReply::Exists | RequestNameReply::InQueue) | Err(zbus::Error::NameTaken) => {
            call(&conn, first)?;
            Ok(Claim::Forwarded)
        }
        Err(e) => Err(e),
    }
}

/// Sends a command to the running GUI. `Ok(false)` if none runs. Uses the session bus.
pub fn send(cmd: &Command) -> zbus::Result<bool> {
    send_on(&Connection::session()?, cmd)
}

/// [`send`] on a given bus connection.
pub fn send_on(conn: &Connection, cmd: &Command) -> zbus::Result<bool> {
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn)?;
    let name = zbus::names::BusName::try_from(BUS_NAME)?;
    if !dbus.name_has_owner(name)? {
        return Ok(false);
    }
    call(conn, cmd)?;
    Ok(true)
}
