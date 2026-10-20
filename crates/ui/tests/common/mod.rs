//! A private D-Bus daemon for tests (they skip themselves without `dbus-daemon`).

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use zbus::blocking::{Connection, connection};

pub struct Bus(Child, pub String);

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn private_bus() -> Option<Bus> {
    let mut child = Command::new("dbus-daemon")
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

pub fn connect(bus: &Bus) -> Connection {
    connection::Builder::address(bus.1.as_str())
        .unwrap()
        .build()
        .unwrap()
}
