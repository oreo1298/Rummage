//! Applying index settings to a user daemon: the file is written, the daemon reloads, and a
//! rejected file is rolled back.

use std::io::BufReader;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::mpsc;

use fossick_client::Target;
use fossick_proto::{
    ErrorKind, PROTOCOL_VERSION, Request, Response, ServerMode, read_message, write_message,
};
use fossick_ui::daemon_config::IndexSettings;

/// A daemon that answers Reload with `ok` and reports the configuration file’s text as it was
/// when asked to reload.
fn daemon(sock: &Path, file: &Path, ok: bool) -> mpsc::Receiver<String> {
    let listener = UnixListener::bind(sock).unwrap();
    let file = file.to_owned();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            while let Ok(req) = read_message::<Request>(&mut reader) {
                let reply = match req {
                    Request::Hello { .. } => Response::Welcome {
                        version: PROTOCOL_VERSION,
                        server: "mock".into(),
                        mode: ServerMode::User,
                        uid: 1000,
                    },
                    Request::Reload => {
                        let _ = tx.send(std::fs::read_to_string(&file).unwrap_or_default());
                        if ok {
                            Response::Ok
                        } else {
                            Response::Error {
                                id: None,
                                kind: ErrorKind::BadConfig,
                                message: "bad volume".into(),
                            }
                        }
                    }
                    _ => Response::Ok,
                };
                if write_message(&mut writer, &reply).is_err() {
                    break;
                }
            }
        }
    });
    rx
}

#[test]
fn applies_and_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config/fossick/fossickd.toml");
    let settings = IndexSettings {
        exclude_names: vec!["node_modules".into()],
        ..IndexSettings::from_text("").unwrap()
    };

    // No file yet: it is created and the daemon reloads it.
    let sock = dir.path().join("ok.sock");
    let reloads = daemon(&sock, &file, true);
    settings.save_user(&file, &Target::Socket(sock)).unwrap();
    let seen = reloads.recv().unwrap();
    assert!(seen.contains("node_modules"), "{seen}");
    assert_eq!(IndexSettings::load_from(&file).unwrap(), settings);

    // A rejected change leaves the previous file in place.
    let sock = dir.path().join("bad.sock");
    let reloads = daemon(&sock, &file, false);
    let before = std::fs::read_to_string(&file).unwrap();
    let worse = IndexSettings {
        volumes: vec!["/nope".into()],
        ..settings.clone()
    };
    let err = worse.save_user(&file, &Target::Socket(sock)).unwrap_err();
    assert!(err.contains("bad volume"), "{err}");
    assert!(reloads.recv().unwrap().contains("/nope"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);

    // Without a daemon a new file is removed again.
    let fresh = dir.path().join("other/fossickd.toml");
    assert!(
        settings
            .save_user(&fresh, &Target::Socket(dir.path().join("none")))
            .is_err()
    );
    assert!(!fresh.exists());
}
