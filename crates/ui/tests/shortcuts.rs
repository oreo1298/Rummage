//! The global shortcut against a scripted GlobalShortcuts portal on a private D-Bus daemon
//! (skipped without `dbus-daemon`).

mod common;

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Bus, connect, private_bus};
use fossick_ui::shortcuts::{GlobalShortcut, Status, TOGGLE_ID};
use zbus::blocking::{Connection, connection};
use zbus::message::Header;
use zbus::names::BusName;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const PATH: &str = "/org/freedesktop/portal/desktop";

/// What the scripted portal saw.
#[derive(Default)]
struct Log {
    app_id: String,
    preferred: String,
    configured: bool,
    closed: bool,
}

struct Portal {
    /// The Response code BindShortcuts answers with (0 = bound, 1 = declined).
    code: u32,
    log: Arc<Mutex<Log>>,
}

fn text(options: &HashMap<String, OwnedValue>, key: &str) -> String {
    options
        .get(key)
        .and_then(|v| v.downcast_ref::<String>().ok())
        .unwrap_or_default()
}

fn sender_part(hdr: &Header<'_>) -> String {
    hdr.sender()
        .map(|s| s.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default()
}

async fn respond(
    conn: &zbus::Connection,
    hdr: &Header<'_>,
    handle: &str,
    code: u32,
    results: HashMap<&str, Value<'_>>,
) -> zbus::fdo::Result<OwnedObjectPath> {
    let path = format!("{PATH}/request/{}/{handle}", sender_part(hdr));
    conn.emit_signal(
        hdr.sender().map(zbus::names::UniqueName::as_str),
        path.as_str(),
        "org.freedesktop.portal.Request",
        "Response",
        &(code, results),
    )
    .await?;
    Ok(OwnedObjectPath::try_from(path).unwrap())
}

#[zbus::interface(name = "org.freedesktop.portal.GlobalShortcuts")]
impl Portal {
    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let session = format!(
            "{PATH}/session/{}/{}",
            sender_part(&hdr),
            text(&options, "session_handle_token")
        );
        conn.object_server()
            .at(
                session.as_str(),
                Session {
                    log: self.log.clone(),
                },
            )
            .await?;
        let results = HashMap::from([("session_handle", Value::from(session.as_str()))]);
        respond(conn, &hdr, &text(&options, "handle_token"), 0, results).await
    }

    async fn bind_shortcuts(
        &self,
        _session: OwnedObjectPath,
        shortcuts: Vec<(String, HashMap<String, OwnedValue>)>,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let (id, props) = &shortcuts[0];
        assert_eq!(id, TOGGLE_ID);
        assert!(!text(props, "description").is_empty());
        self.log.lock().unwrap().preferred = text(props, "preferred_trigger");
        let bound = vec![(
            TOGGLE_ID,
            HashMap::from([
                ("description", Value::from("Show or hide")),
                ("trigger_description", Value::from("Ctrl+Alt+Space")),
            ]),
        )];
        let results = HashMap::from([("shortcuts", Value::from(bound))]);
        respond(
            conn,
            &hdr,
            &text(&options, "handle_token"),
            self.code,
            results,
        )
        .await
    }

    fn configure_shortcuts(
        &self,
        _session: OwnedObjectPath,
        _parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) {
        self.log.lock().unwrap().configured = true;
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        2
    }
}

struct Registry {
    log: Arc<Mutex<Log>>,
}

#[zbus::interface(name = "org.freedesktop.host.portal.Registry")]
impl Registry {
    fn register(&self, app_id: String, _options: HashMap<String, OwnedValue>) {
        self.log.lock().unwrap().app_id = app_id;
    }
}

struct Session {
    log: Arc<Mutex<Log>>,
}

#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl Session {
    fn close(&self) {
        self.log.lock().unwrap().closed = true;
    }
}

fn portal(bus: &Bus, code: u32) -> (Connection, Arc<Mutex<Log>>) {
    let log = Arc::new(Mutex::new(Log::default()));
    let conn = connection::Builder::address(bus.1.as_str())
        .unwrap()
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .serve_at(
            PATH,
            Portal {
                code,
                log: log.clone(),
            },
        )
        .unwrap()
        .serve_at(PATH, Registry { log: log.clone() })
        .unwrap()
        .build()
        .unwrap();
    (conn, log)
}

fn activate(portal: &Connection, session: &str, id: &str) {
    portal
        .emit_signal(
            None::<BusName<'_>>,
            PATH,
            "org.freedesktop.portal.GlobalShortcuts",
            "Activated",
            &(
                ObjectPath::try_from(session).unwrap(),
                id,
                0_u64,
                HashMap::from([("activation_token", Value::from("tok"))]),
            ),
        )
        .unwrap();
}

#[test]
fn binds_and_reports_presses() {
    let Some(bus) = private_bus() else {
        eprintln!("no dbus-daemon: skipped");
        return;
    };
    let (portal_conn, log) = portal(&bus, 0);
    let (status_tx, status_rx) = mpsc::channel();
    let (press_tx, press_rx) = mpsc::channel();
    let client = connect(&bus);
    let session_prefix = format!(
        "{PATH}/session/{}/",
        client
            .unique_name()
            .unwrap()
            .trim_start_matches(':')
            .replace('.', "_")
    );
    let shortcut = GlobalShortcut::start_on(
        client,
        "io.github.oreo1298.Fossick",
        "CTRL+ALT+space",
        "Show or hide the search window",
        move |s| {
            let _ = status_tx.send(s);
        },
        move |token| {
            let _ = press_tx.send(token);
        },
    )
    .unwrap();
    let wait = Duration::from_secs(5);
    assert_eq!(
        status_rx.recv_timeout(wait).unwrap(),
        Status::Bound {
            trigger: "Ctrl+Alt+Space".into()
        }
    );
    {
        let log = log.lock().unwrap();
        assert_eq!(log.app_id, "io.github.oreo1298.Fossick");
        assert_eq!(log.preferred, "CTRL+ALT+space");
    }

    // Find our session path: the only one registered under our sender.
    // The session token is private to the shortcut; find the session in the portal’s tree.
    let session = {
        let proxy = zbus::blocking::fdo::IntrospectableProxy::builder(&connect(&bus))
            .destination("org.freedesktop.portal.Desktop")
            .unwrap()
            .path(session_prefix.trim_end_matches('/'))
            .unwrap()
            .build()
            .unwrap();
        let xml = proxy.introspect().unwrap();
        let node = xml
            .split("<node name=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("a session node")
            .to_owned();
        format!("{session_prefix}{node}")
    };

    // Presses of other sessions or other shortcuts are ignored; ours carry the token.
    activate(&portal_conn, &format!("{PATH}/session/other/x"), TOGGLE_ID);
    activate(&portal_conn, &session, "something-else");
    activate(&portal_conn, &session, TOGGLE_ID);
    assert_eq!(press_rx.recv_timeout(wait).unwrap(), "tok");
    assert!(press_rx.recv_timeout(Duration::from_millis(200)).is_err());

    shortcut.configure().unwrap();
    assert!(log.lock().unwrap().configured);

    // Dropping closes the session and stops the listener.
    drop(shortcut);
    assert!(log.lock().unwrap().closed);
    activate(&portal_conn, &session, TOGGLE_ID);
    assert!(press_rx.recv_timeout(Duration::from_millis(200)).is_err());
}

#[test]
fn declining_and_missing_portals_are_reported() {
    let Some(bus) = private_bus() else {
        eprintln!("no dbus-daemon: skipped");
        return;
    };
    let wait = Duration::from_secs(5);

    // No portal on the bus.
    let (tx, rx) = mpsc::channel();
    let _none = GlobalShortcut::start_on(
        connect(&bus),
        "io.github.oreo1298.Fossick",
        "",
        "Show or hide",
        move |s| {
            let _ = tx.send(s);
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(
        rx.recv_timeout(wait).unwrap(),
        Status::Unavailable("the desktop has no GlobalShortcuts portal".into())
    );

    // The user says no in the desktop’s dialog.
    let (_portal, _log) = portal(&bus, 1);
    let (tx, rx) = mpsc::channel();
    let _declined = GlobalShortcut::start_on(
        connect(&bus),
        "",
        "",
        "Show or hide",
        move |s| {
            let _ = tx.send(s);
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(rx.recv_timeout(wait).unwrap(), Status::Cancelled);
}
