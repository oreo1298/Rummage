//! Single instance over a private D-Bus daemon (skipped without `dbus-daemon`).

mod common;

use std::sync::mpsc;
use std::time::Duration;

use common::{connect, private_bus};
use fossick_ui::instance::{Claim, Command, claim_on, send_on};

#[test]
fn second_instances_forward_their_command() {
    let Some(bus) = private_bus() else {
        eprintln!("no dbus-daemon: skipped");
        return;
    };
    let (tx, rx) = mpsc::channel();
    let first = claim_on(
        connect(&bus),
        &Command::Activate {
            token: String::new(),
        },
        move |c| {
            let _ = tx.send(c);
        },
    )
    .unwrap();
    assert!(matches!(first, Claim::Primary(_)));

    let search = Command::Search {
        text: "report".into(),
        token: "tok".into(),
    };
    let second = claim_on(connect(&bus), &search, |_| {}).unwrap();
    assert!(matches!(second, Claim::Forwarded));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), search);

    let toggle = Command::Toggle { token: "t2".into() };
    assert!(send_on(&connect(&bus), &toggle).unwrap());
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), toggle);
    let settings = Command::Settings { token: "t3".into() };
    assert!(send_on(&connect(&bus), &settings).unwrap());
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), settings);

    // Once the GUI is gone, nobody answers.
    drop(first);
    std::thread::sleep(Duration::from_millis(200));
    assert!(!send_on(&connect(&bus), &toggle).unwrap());
}
