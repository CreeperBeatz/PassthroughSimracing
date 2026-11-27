//! UDP datagrams from the headset to `green_patch.py` on the rig PC (port `PATCH_UDP_PORT`, 9945): the
//! green cap's states and its `get` from the tuning menu (tuning_mode.rs), and the QR code shown while the
//! play area is edited (`AreaQr`). The streamer is not involved: the rig PC keeps its streamer build.
//!
//! - `debug.alvr.patch_host <ip>` or `<ip>:<port>`: where to send (unset: the streamer's address of the
//!   last connection, port 9945). Read about once a second; the destination is logged when it changes.
//!   A wired stream (USB, adb forwarding) has the streamer at a loopback address, and adb reverse does
//!   not carry UDP: then nothing is sent until `patch_host` names the rig PC's LAN address (the
//!   headset's Wi-Fi on the same network; the stream itself may stay wired).
//!
//! The socket is made on the first message, non-blocking; replies are read each frame while tuning.

use crate::passthrough::debug_property_string;
use alvr_graphics::{PATCH_UDP_PORT, qr_message};
use std::{
    collections::VecDeque,
    net::{IpAddr, SocketAddr, UdpSocket},
    time::{Duration, Instant},
};

const RESOLVE_INTERVAL: Duration = Duration::from_secs(1);
// Replies read per poll at most
const MAX_REPLIES: usize = 16;

/// Why nothing is sent while the stream is wired
pub const WIRED_TEXT: &str =
    "streamer is wired (loopback); set debug.alvr.patch_host to the rig PC's LAN IP";

/// Where the datagrams go: `debug.alvr.patch_host` (an IP address, with or without a port), else the
/// streamer's address with `PATCH_UDP_PORT` unless it is a loopback one (`WIRED_TEXT`). Err: why there
/// is none (the text for the log)
pub fn destination(
    host: Option<&str>,
    server: Option<IpAddr>,
) -> Result<Option<SocketAddr>, String> {
    match host.map(str::trim).filter(|host| !host.is_empty()) {
        Some(host) => host
            .parse::<SocketAddr>()
            .or_else(|_| {
                host.parse::<IpAddr>()
                    .map(|ip| SocketAddr::new(ip, PATCH_UDP_PORT))
            })
            .map(Some)
            .map_err(|_| format!("debug.alvr.patch_host {host:?} is no IP address (or ip:port)")),
        None => match server {
            Some(ip) if ip.is_loopback() => Err(WIRED_TEXT.into()),
            server => Ok(server.map(|ip| SocketAddr::new(ip, PATCH_UDP_PORT))),
        },
    }
}

pub struct RigLink {
    socket: Option<UdpSocket>,
    // The socket could not be made: not tried again
    socket_failed: bool,
    destination: Option<SocketAddr>,
    last_resolve: Option<Instant>,
    // What the log said last: the destination, or why there is none
    logged: Option<Result<Option<SocketAddr>, String>>,
    // A send failed: logged once until one works again
    send_failed: bool,
    // The stream is wired and `patch_host` is unset
    wired: bool,
}

impl RigLink {
    pub fn new() -> Self {
        Self {
            socket: None,
            socket_failed: false,
            destination: None,
            last_resolve: None,
            logged: None,
            send_failed: false,
            wired: false,
        }
    }

    /// A short hint for the tuning menu when nothing can be sent: the stream is wired and
    /// `debug.alvr.patch_host` is unset
    pub fn hint(&self) -> Option<&'static str> {
        self.wired
            .then_some("wired stream: setprop debug.alvr.patch_host <rig PC LAN IP>")
    }

    fn resolve(&mut self) -> Option<SocketAddr> {
        if self
            .last_resolve
            .is_none_or(|time| time.elapsed() >= RESOLVE_INTERVAL)
        {
            self.last_resolve = Some(Instant::now());
            let result = destination(
                debug_property_string("debug.alvr.patch_host").as_deref(),
                alvr_client_core::server_ip(),
            );
            if self.logged.as_ref() != Some(&result) {
                crate::stream_log(&match &result {
                    Ok(Some(address)) => format!("rig: green_patch.py messages go to {address}"),
                    Ok(None) => "rig: no address for green_patch.py yet (no streamer connected, \
                                 debug.alvr.patch_host unset)"
                        .to_string(),
                    Err(e) => format!("rig: {e}"),
                });
                self.logged = Some(result.clone());
            }
            self.wired = result.as_ref().is_err_and(|e| e == WIRED_TEXT);
            self.destination = result.ok().flatten();
        }
        self.destination
    }

    fn socket(&mut self) -> Option<&UdpSocket> {
        if self.socket.is_none() && !self.socket_failed {
            match UdpSocket::bind("0.0.0.0:0").and_then(|socket| {
                socket.set_nonblocking(true)?;
                Ok(socket)
            }) {
                Ok(socket) => self.socket = Some(socket),
                Err(e) => {
                    self.socket_failed = true;
                    crate::stream_log(&format!("rig: no UDP socket: {e}"));
                }
            }
        }
        self.socket.as_ref()
    }

    /// Sends `message` (nothing without a destination). Whether it went out
    pub fn send(&mut self, message: &str) -> bool {
        let Some(destination) = self.resolve() else {
            return false;
        };
        let Some(socket) = self.socket() else {
            return false;
        };
        match socket.send_to(message.as_bytes(), destination) {
            Ok(_) => {
                self.send_failed = false;
                true
            }
            Err(e) => {
                if !self.send_failed {
                    self.send_failed = true;
                    crate::stream_log(&format!(
                        "rig: sending to {destination} failed: {e} (not logged again until a send \
                         works)"
                    ));
                }
                false
            }
        }
    }

    /// The datagrams that came in since the last call (as text), without waiting
    pub fn poll(&mut self) -> Vec<String> {
        let mut replies = Vec::new();
        let Some(socket) = &self.socket else {
            return replies;
        };
        let mut buffer = [0_u8; 2048];
        while replies.len() < MAX_REPLIES {
            match socket.recv_from(&mut buffer) {
                Ok((length, _)) => {
                    replies.push(String::from_utf8_lossy(&buffer[..length]).into_owned());
                }
                // WouldBlock: nothing more; other errors (a refused port on some systems): the same
                Err(_) => break,
            }
        }
        replies
    }
}

/// Each request is sent this often, this far apart (seconds): UDP may drop one
pub const QR_BURST: usize = 3;
pub const QR_BURST_SPACING_S: f64 = 0.1;
/// While the area is edited the request to show is sent again this often (a tool started late)
pub const QR_REPEAT_S: f64 = 2.0;

/// What `AreaQr::update` gives: the messages due now, and a line for the log when the request changed
#[derive(Debug, Default, PartialEq)]
pub struct QrStep {
    pub messages: Vec<String>,
    pub log: Option<String>,
}

/// The QR code on the rig monitor while the play area is edited (any way it was opened): show requests
/// while editing (with a lease, `QR_LEASE_S`), a hide request when it ends
#[derive(Default)]
pub struct AreaQr {
    // The payload asked for while editing
    shown: Option<String>,
    next_repeat: f64,
    pending: VecDeque<(f64, String)>,
}

impl AreaQr {
    fn burst(&mut self, now: f64, message: &str) {
        for index in 0..QR_BURST {
            self.pending
                .push_back((now + index as f64 * QR_BURST_SPACING_S, message.to_owned()));
        }
    }

    /// One frame: `editing`, the payload the headset looks for, `now` (seconds)
    pub fn update(&mut self, editing: bool, payload: &str, now: f64) -> QrStep {
        let mut log = None;
        if editing {
            if self.shown.as_deref() != Some(payload) {
                self.shown = Some(payload.to_owned());
                self.pending.clear();
                self.burst(now, &qr_message(payload, true));
                self.next_repeat = now + QR_REPEAT_S;
                log = Some(format!("area: qr show {payload}"));
            } else if now >= self.next_repeat {
                self.burst(now, &qr_message(payload, true));
                self.next_repeat = now + QR_REPEAT_S;
            }
        } else if let Some(shown) = self.shown.take() {
            self.pending.clear();
            self.burst(now, &qr_message(&shown, false));
            log = Some("area: qr hide".to_string());
        }

        let mut messages = Vec::new();
        while let Some((time, _)) = self.pending.front()
            && *time <= now
        {
            if let Some((_, message)) = self.pending.pop_front() {
                messages.push(message);
            }
        }
        QrStep { messages, log }
    }

    /// The session stops: the hide request to send at once (one), if the code is asked for. The PC also
    /// hides it when the lease of the last show request runs out
    pub fn hide_now(&mut self) -> Option<String> {
        let shown = self.shown.take()?;
        self.pending.clear();
        Some(qr_message(&shown, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_from_the_property_or_the_streamer() {
        let server: IpAddr = "192.168.1.20".parse().unwrap();
        assert_eq!(
            destination(None, Some(server)),
            Ok(Some("192.168.1.20:9945".parse().unwrap()))
        );
        assert_eq!(destination(None, None), Ok(None));
        assert_eq!(
            destination(Some(" 10.0.0.5 "), Some(server)),
            Ok(Some("10.0.0.5:9945".parse().unwrap()))
        );
        assert_eq!(
            destination(Some("10.0.0.5:7000"), None),
            Ok(Some("10.0.0.5:7000".parse().unwrap()))
        );
        assert_eq!(
            destination(Some(""), Some(server)),
            Ok(Some("192.168.1.20:9945".parse().unwrap()))
        );
        assert!(destination(Some("rig-pc"), Some(server)).is_err());
        // Wired (adb forwarding): not the loopback address, unless the property names one
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(
            destination(None, Some(loopback)),
            Err(WIRED_TEXT.to_string())
        );
        assert_eq!(
            destination(Some("192.168.8.3"), Some(loopback)),
            Ok(Some("192.168.8.3:9945".parse().unwrap()))
        );
        assert_eq!(
            destination(Some("127.0.0.1:9945"), Some(loopback)),
            Ok(Some("127.0.0.1:9945".parse().unwrap()))
        );
    }

    #[test]
    fn qr_requests_follow_the_editor() {
        let mut qr = AreaQr::default();
        let show = qr_message("SCR1", true);
        let hide = qr_message("SCR1", false);
        // Not editing: nothing
        assert_eq!(qr.update(false, "SCR1", 0.0), QrStep::default());
        // Editing starts: the first at once, then 100 ms apart, logged once
        let step = qr.update(true, "SCR1", 1.0);
        assert_eq!(step.messages, vec![show.clone()]);
        assert_eq!(step.log.as_deref(), Some("area: qr show SCR1"));
        assert!(qr.update(true, "SCR1", 1.05).messages.is_empty());
        assert_eq!(qr.update(true, "SCR1", 1.1).messages, vec![show.clone()]);
        let step = qr.update(true, "SCR1", 1.25);
        assert_eq!(step.messages, vec![show.clone()]);
        assert_eq!(step.log, None);
        // Repeated every 2 s while editing
        assert!(qr.update(true, "SCR1", 2.9).messages.is_empty());
        let repeated = [3.0, 3.1, 3.2]
            .iter()
            .flat_map(|&t| qr.update(true, "SCR1", t).messages)
            .collect::<Vec<_>>();
        assert_eq!(repeated, vec![show.clone(); 3]);
        // Editing ends: hide, three times, logged once
        let step = qr.update(false, "SCR1", 4.0);
        assert_eq!(step.messages, vec![hide.clone()]);
        assert_eq!(step.log.as_deref(), Some("area: qr hide"));
        let rest = [4.1, 4.2, 4.3, 9.0]
            .iter()
            .flat_map(|&t| qr.update(false, "SCR1", t).messages)
            .collect::<Vec<_>>();
        assert_eq!(rest, vec![hide; 2]);
        // A frame that skips ahead sends what is due at once
        qr.update(true, "B", 10.0);
        assert_eq!(qr.update(true, "B", 10.5).messages.len(), 2);
        // The session stops while editing: one hide at once, nothing more after
        assert_eq!(qr.hide_now(), Some(qr_message("B", false)));
        assert_eq!(qr.hide_now(), None);
        assert_eq!(qr.update(false, "B", 11.0), QrStep::default());
    }
}
