//! FT8 slot timing and a tiny SNTP client for checking the computer's clock.
//!
//! FT8 needs the clock within about a second of the other station; WSJT-X users usually keep it
//! within 0.1-0.2 s. We measure the offset at startup and warn if it's off.

use std::net::{ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SLOT_SECONDS: f64 = 15.0;

/// Correction (seconds) subtracted from the system clock, stored as f64 bits.
static CORRECTION: AtomicU64 = AtomicU64::new(0);

/// Raw system clock, seconds since the Unix epoch.
pub fn system_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Corrected time: the system clock minus the measured NTP offset (if any).
/// Everything in CQ DJ that cares about FT8 timing uses this.
pub fn now_secs() -> f64 {
    system_secs() - f64::from_bits(CORRECTION.load(Ordering::Relaxed))
}

/// Sets the clock correction (positive = the system clock is ahead).
pub fn set_correction(offset_secs: f64) {
    CORRECTION.store(offset_secs.to_bits(), Ordering::Relaxed);
}

pub fn correction() -> f64 {
    f64::from_bits(CORRECTION.load(Ordering::Relaxed))
}

/// Start time (Unix seconds) of the slot containing `t`.
pub fn slot_start(t: f64) -> f64 {
    (t / SLOT_SECONDS).floor() * SLOT_SECONDS
}

/// How far into its slot `t` is, in seconds (0.0..15.0).
pub fn seconds_into_slot(t: f64) -> f64 {
    t - slot_start(t)
}

/// Which half of the minute a slot belongs to. FT8 stations take turns:
/// "Even" slots start at :00 and :30, "Odd" slots at :15 and :45.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Parity {
    Even,
    Odd,
}

impl Parity {
    pub fn of_slot(slot_start_secs: f64) -> Parity {
        let index = (slot_start_secs / SLOT_SECONDS).round() as i64;
        if index.rem_euclid(2) == 0 {
            Parity::Even
        } else {
            Parity::Odd
        }
    }
    pub fn other(self) -> Parity {
        match self {
            Parity::Even => Parity::Odd,
            Parity::Odd => Parity::Even,
        }
    }
}

/// HHMMSS label for a slot start, like WSJT-X shows.
pub fn utc_label(t: f64) -> String {
    let s = t.floor() as i64;
    let day = s.rem_euclid(86_400);
    format!("{:02}{:02}{:02}", day / 3600, (day % 3600) / 60, day % 60)
}

/// Measures how far the computer clock is from an NTP server, in seconds.
/// Positive means the computer is ahead. Uses one SNTP request (RFC 4330).
pub fn measure_clock_offset(server: &str, timeout: Duration) -> std::io::Result<f64> {
    let addr = (server, 123)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no address for NTP server"))?;
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(timeout))?;

    let mut req = [0u8; 48];
    req[0] = 0b00_100_011; // LI 0, version 4, mode 3 (client)
    let t1 = system_secs();
    socket.send_to(&req, addr)?;
    let mut buf = [0u8; 48];
    let (n, _) = socket.recv_from(&mut buf)?;
    let t4 = system_secs();
    if n < 48 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "short NTP reply"));
    }
    let t2 = ntp_timestamp(&buf[32..40]); // server receive time
    let t3 = ntp_timestamp(&buf[40..48]); // server transmit time
    // Standard NTP offset: how far the server is ahead of us; negate so positive = we're ahead.
    let server_ahead = ((t2 - t1) + (t3 - t4)) / 2.0;
    Ok(-server_ahead)
}

fn ntp_timestamp(b: &[u8]) -> f64 {
    const NTP_TO_UNIX: f64 = 2_208_988_800.0;
    let secs = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64;
    let frac = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as f64 / 4_294_967_296.0;
    secs + frac - NTP_TO_UNIX
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_math() {
        assert_eq!(slot_start(1_000_000_007.5), 1_000_000_005.0);
        assert!((seconds_into_slot(1_000_000_007.5) - 2.5).abs() < 1e-9);
        assert_eq!(Parity::of_slot(0.0), Parity::Even);
        assert_eq!(Parity::of_slot(15.0), Parity::Odd);
        assert_eq!(Parity::of_slot(30.0), Parity::Even);
        assert_eq!(Parity::Even.other(), Parity::Odd);
        assert_eq!(utc_label(3600.0 * 14.0 + 60.0 * 32.0 + 15.0), "143215");
    }

    #[test]
    fn parses_ntp_timestamps() {
        // 2026-01-01T00:00:00Z = Unix 1767225600 = NTP 3976214400, plus half a second
        let mut b = [0u8; 8];
        b[..4].copy_from_slice(&3_976_214_400u32.to_be_bytes());
        b[4..].copy_from_slice(&0x8000_0000u32.to_be_bytes());
        assert!((ntp_timestamp(&b) - 1_767_225_600.5).abs() < 1e-6);
    }
}
