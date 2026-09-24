//! Headless protocol smoke test — talks to the instrument (or the mock)
//! without any GUI. Usage:
//!
//!     cargo run --example probe -- 127.0.0.1:15000
//!     cargo run --example probe -- 192.168.1.5:5000

use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::Duration;

use cryocon_gui::device::{DeviceCmd, DeviceEvent, DeviceReply, Request, Worker};

fn main() {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:15000".into());
    let (host, port) = addr
        .rsplit_once(':')
        .expect("address must look like host:port");

    let (req_tx, req_rx) = channel();
    let (ev_tx, ev_rx) = channel();
    Worker::spawn(
        host.to_string(),
        port.parse().expect("port must be a number"),
        req_rx,
        ev_tx,
    );

    // wait for the worker to connect (it announces itself with *IDN?)
    let idn = loop {
        match ev_rx.recv_timeout(Duration::from_secs(8)) {
            Ok(DeviceEvent::Connected(idn)) => {
                println!("connected: {idn}");
                break idn;
            }
            Ok(other) => println!("event: {other:?}"),
            Err(RecvTimeoutError::Timeout) => {
                eprintln!("no connection within 8 s — is the mock/instrument up?");
                std::process::exit(1);
            }
            Err(RecvTimeoutError::Disconnected) => std::process::exit(1),
        }
    };
    assert!(idn.contains("22C") || idn.contains("MODEL22C"), "unexpected device");

    // one synchronous request helper (same pattern the UI uses)
    let ask = |cmd: DeviceCmd| -> DeviceReply {
        let (tx, rx) = channel();
        req_tx.send(Request { cmd, reply: tx }).unwrap();
        rx.recv_timeout(Duration::from_secs(3)).unwrap()
    };

    match ask(DeviceCmd::Poll) {
        DeviceReply::Snapshot(s) => {
            let show = |name: &str, v: Option<f64>| match v {
                Some(x) => println!("{name}: {x:.3}"),
                None => println!("{name}: no reading"),
            };
            show("input A, K", s.t_a);
            show("input B, K", s.t_b);
            show("setpoint 1, K", s.setpoint_1);
            show("rate 1, K/min", s.rate_1);
            show("heater power, %", s.power_1);
        }
        other => println!("poll failed: {other:?}"),
    }

    // clean shutdown of the worker
    let (tx, _rx) = channel();
    let _ = req_tx.send(Request { cmd: DeviceCmd::Shutdown, reply: tx });
    println!("probe OK");
}
