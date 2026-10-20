//! Single instance over a private D-Bus daemon (skipped without `dbus-daemon`).

use std::io::{BufRead, BufReader};
use std::process::{Child, Command as Process, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use fossick_ui::instance::{Claim, Command, claim_on, send_on};
use zbus::blocking::{Connection, connection};

struct Bus(Child, String);

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn private_bus() -> Option<Bus> {
    let mut child = Process::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take()?)
        .read_line(&mut line)
        .ok()?;
    Some(Bus(child, line.trim().to_owned()))
}

fn connect(bus: &Bus) -> Connection {
    connection::Builder::address(bus.1.as_str())
        .unwrap()
        .build()
        .unwrap()
}

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

    // Once the GUI is gone, nobody answers.
    drop(first);
    std::thread::sleep(Duration::from_millis(200));
    assert!(!send_on(&connect(&bus), &toggle).unwrap());
}
