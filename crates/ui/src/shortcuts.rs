//! The global shortcut that shows or hides the search window, through the GlobalShortcuts
//! desktop portal (KDE Plasma 6, GNOME 48 and later, Hyprland, …). The desktop asks the user to
//! confirm the shortcut, and they can change it later in the desktop’s own settings. Without
//! the portal, bind `fossick --toggle` to a key in the desktop’s settings instead.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const REQUEST: &str = "org.freedesktop.portal.Request";
const REGISTRY: &str = "org.freedesktop.host.portal.Registry";

/// The id of the one shortcut Fossick registers.
pub const TOGGLE_ID: &str = "toggle-window";

/// What became of the binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The shortcut works; `trigger` describes its keys as the desktop shows them (may be
    /// empty when the user hasn’t assigned keys yet).
    Bound { trigger: String },
    /// The user declined in the desktop’s dialog.
    Cancelled,
    /// No portal, or it failed.
    Unavailable(String),
}

/// The shortcut, registered while this lives.
pub struct GlobalShortcut {
    conn: Connection,
    session: std::sync::Arc<std::sync::Mutex<Option<OwnedObjectPath>>>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for GlobalShortcut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GlobalShortcut")
    }
}

type Results = HashMap<String, OwnedValue>;

/// The `sender` part of portal request and session paths.
fn sender_part(conn: &Connection) -> String {
    conn.unique_name()
        .map(|n| n.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default()
}

fn token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "fossick{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Calls a portal method that answers through a Request object’s `Response` signal. The
/// signal is subscribed to before the call, so it can’t be missed.
fn request<B>(
    conn: &Connection,
    method: &str,
    body: &B,
    handle_token: &str,
) -> zbus::Result<(u32, Results)>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let path = format!("{PORTAL_PATH}/request/{}/{handle_token}", sender_part(conn));
    let req = Proxy::new(conn, PORTAL, path, REQUEST)?;
    let mut responses = req.receive_signal("Response")?;
    let portal = Proxy::new(conn, PORTAL, PORTAL_PATH, SHORTCUTS)?;
    let _: OwnedObjectPath = portal.call(method, body)?;
    let msg = responses
        .next()
        .ok_or_else(|| zbus::Error::Failure("the portal went away".into()))?;
    msg.body().deserialize()
}

/// A string value, looking through variants.
fn as_string(v: &Value<'_>) -> Option<String> {
    match v {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        Value::Value(v) => as_string(v),
        _ => None,
    }
}

fn string(results: &Results, key: &str) -> Option<String> {
    as_string(results.get(key)?)
}

/// The trigger description of the toggle shortcut in a BindShortcuts response.
fn bound_trigger(results: &Results) -> String {
    let mut list = results.get("shortcuts").map(|v| &**v);
    while let Some(Value::Value(v)) = list {
        list = Some(v);
    }
    let Some(Value::Array(list)) = list else {
        return String::new();
    };
    for item in list.inner() {
        let Value::Structure(s) = item else { continue };
        let [Value::Str(id), Value::Dict(props)] = s.fields() else {
            continue;
        };
        if id.as_str() == TOGGLE_ID {
            return props
                .iter()
                .find(|(k, _)| as_string(k).as_deref() == Some("trigger_description"))
                .and_then(|(_, v)| as_string(v))
                .unwrap_or_default();
        }
    }
    String::new()
}

/// Opens the session and binds the shortcut. `Ok(None)` if the user declined.
fn bind(
    conn: &Connection,
    app_id: &str,
    preferred: &str,
    description: &str,
) -> zbus::Result<Option<(OwnedObjectPath, String)>> {
    // Host (unsandboxed) apps name themselves to the portal first; older portals lack the
    // registry and fall back to guessing the app from its systemd unit.
    if !app_id.is_empty()
        && let Ok(registry) = Proxy::new(conn, PORTAL, PORTAL_PATH, REGISTRY)
    {
        let _: zbus::Result<()> =
            registry.call("Register", &(app_id, HashMap::<&str, Value<'_>>::new()));
    }

    let handle = token();
    let session_token = token();
    let options = HashMap::from([
        ("handle_token", Value::from(handle.as_str())),
        ("session_handle_token", Value::from(session_token.as_str())),
    ]);
    let (code, results) = request(conn, "CreateSession", &(options,), &handle)?;
    if code == 1 {
        return Ok(None);
    }
    if code != 0 {
        return Err(zbus::Error::Failure("the portal refused a session".into()));
    }
    let session = string(&results, "session_handle")
        .ok_or_else(|| zbus::Error::Failure("no session handle".into()))?;
    let session = OwnedObjectPath::try_from(session)?;

    let handle = token();
    let mut props = HashMap::from([("description", Value::from(description))]);
    if !preferred.is_empty() {
        props.insert("preferred_trigger", Value::from(preferred));
    }
    let shortcuts = vec![(TOGGLE_ID, props)];
    let options = HashMap::from([("handle_token", Value::from(handle.as_str()))]);
    let (code, results) = request(
        conn,
        "BindShortcuts",
        &(ObjectPath::from(&session), shortcuts, "", options),
        &handle,
    )?;
    match code {
        0 => Ok(Some((session, bound_trigger(&results)))),
        1 => Ok(None),
        _ => Err(zbus::Error::Failure(
            "the portal refused the shortcut".into(),
        )),
    }
}

impl GlobalShortcut {
    /// Registers the shortcut on the session bus. Binding waits for the user to answer the
    /// desktop’s dialog, so it runs on a thread of its own: `on_status` reports the outcome,
    /// then `on_activated` runs (on that thread) with the activation token, if any, each time
    /// the shortcut is pressed.
    pub fn start(
        app_id: &str,
        preferred: &str,
        description: &str,
        on_status: impl Fn(Status) + Send + 'static,
        on_activated: impl Fn(String) + Send + 'static,
    ) -> zbus::Result<Self> {
        Self::start_on(
            Connection::session()?,
            app_id,
            preferred,
            description,
            on_status,
            on_activated,
        )
    }

    /// [`start`](Self::start) on a given bus connection.
    pub fn start_on(
        conn: Connection,
        app_id: &str,
        preferred: &str,
        description: &str,
        on_status: impl Fn(Status) + Send + 'static,
        on_activated: impl Fn(String) + Send + 'static,
    ) -> zbus::Result<Self> {
        let session = std::sync::Arc::new(std::sync::Mutex::new(None));
        let (app_id, preferred, description) = (
            app_id.to_owned(),
            preferred.to_owned(),
            description.to_owned(),
        );
        let thread = {
            let conn = conn.clone();
            let session = session.clone();
            std::thread::Builder::new()
                .name("fossick-shortcut".into())
                .spawn(move || {
                    // Subscribe before binding, so a press right after the dialog isn’t lost.
                    let activated = Proxy::new(&conn, PORTAL, PORTAL_PATH, SHORTCUTS)
                        .and_then(|p| p.receive_signal("Activated"));
                    let mut activated = match activated {
                        Ok(a) => a,
                        Err(e) => return on_status(Status::Unavailable(describe(&e))),
                    };
                    let mine = match bind(&conn, &app_id, &preferred, &description) {
                        Ok(Some((path, trigger))) => {
                            *session
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some(path.clone());
                            on_status(Status::Bound { trigger });
                            path
                        }
                        Ok(None) => return on_status(Status::Cancelled),
                        Err(e) => return on_status(Status::Unavailable(describe(&e))),
                    };
                    for msg in activated.by_ref() {
                        let Ok((path, id, _time, options)) =
                            msg.body()
                                .deserialize::<(OwnedObjectPath, String, u64, Results)>()
                        else {
                            continue;
                        };
                        if path == mine && id == TOGGLE_ID {
                            on_activated(string(&options, "activation_token").unwrap_or_default());
                        }
                    }
                })
                .map_err(|e| zbus::Error::Failure(e.to_string()))?
        };
        Ok(Self {
            conn,
            session,
            thread: Some(thread),
        })
    }

    /// Opens the desktop’s settings for the shortcut (portal version 2 and later).
    pub fn configure(&self) -> zbus::Result<()> {
        let session = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| zbus::Error::Failure("the shortcut isn’t bound".into()))?;
        let portal = Proxy::new(&self.conn, PORTAL, PORTAL_PATH, SHORTCUTS)?;
        portal.call::<_, _, ()>(
            "ConfigureShortcuts",
            &(
                ObjectPath::from(&session),
                "",
                HashMap::<&str, Value<'_>>::new(),
            ),
        )
    }
}

impl Drop for GlobalShortcut {
    fn drop(&mut self) {
        // Closing the session unregisters the shortcut; closing the connection ends the thread.
        let session = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(path) = session
            && let Ok(s) = Proxy::new(&self.conn, PORTAL, path, "org.freedesktop.portal.Session")
        {
            let _: zbus::Result<()> = s.call("Close", &());
        }
        let _ = self.conn.clone().close();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A short reason, without the D-Bus error name noise for the common case.
fn describe(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"
                || name.as_str() == "org.freedesktop.DBus.Error.UnknownInterface"
                || name.as_str() == "org.freedesktop.DBus.Error.UnknownMethod" =>
        {
            "the desktop has no GlobalShortcuts portal".into()
        }
        e => e.to_string(),
    }
}
