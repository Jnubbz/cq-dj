//! Radio control (CAT). Starts with Yaesu's current command set (FT-710, FT-991A, FTDX10, FT-891):
//! ASCII commands ending in `;` over the radio's USB virtual serial port.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum CatError {
    Io(std::io::Error),
    Serial(String),
    NoReply(&'static str),
    BadReply(String),
}

impl std::fmt::Display for CatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatError::Io(e) => write!(f, "radio connection error: {e}"),
            CatError::Serial(e) => write!(f, "can't open the radio's serial port: {e}"),
            CatError::NoReply(cmd) => write!(f, "the radio didn't answer {cmd} (check the port and CAT baud rate)"),
            CatError::BadReply(r) => write!(f, "unexpected reply from radio: {r}"),
        }
    }
}

impl std::error::Error for CatError {}

impl From<std::io::Error> for CatError {
    fn from(e: std::io::Error) -> Self {
        CatError::Io(e)
    }
}

/// What CQ DJ needs from a radio.
pub trait Rig: Send {
    fn name(&self) -> &str;
    /// Puts the radio in its upper-sideband data mode (DATA-U on Yaesu).
    fn set_data_mode(&mut self) -> Result<(), CatError>;
    fn set_frequency(&mut self, hz: u64) -> Result<(), CatError>;
    fn frequency(&mut self) -> Result<u64, CatError>;
    fn set_ptt(&mut self, transmit: bool) -> Result<(), CatError>;
}

/// No CAT: the user keys with VOX and tunes the radio by hand.
pub struct NoRig;

impl Rig for NoRig {
    fn name(&self) -> &str {
        "No radio control (VOX)"
    }
    fn set_data_mode(&mut self) -> Result<(), CatError> {
        Ok(())
    }
    fn set_frequency(&mut self, _hz: u64) -> Result<(), CatError> {
        Ok(())
    }
    fn frequency(&mut self) -> Result<u64, CatError> {
        Ok(0)
    }
    fn set_ptt(&mut self, _transmit: bool) -> Result<(), CatError> {
        Ok(())
    }
}

/// Yaesu "new" CAT protocol over any byte stream (a serial port in the app, a mock in tests).
pub struct YaesuCat<P: Read + Write + Send> {
    port: P,
    timeout: Duration,
    keyed: bool,
}

impl<P: Read + Write + Send> YaesuCat<P> {
    pub fn new(port: P) -> Self {
        YaesuCat { port, timeout: Duration::from_millis(500), keyed: false }
    }

    fn send(&mut self, cmd: &str) -> Result<(), CatError> {
        self.port.write_all(cmd.as_bytes())?;
        self.port.flush()?;
        Ok(())
    }

    /// Sends a read command and returns the reply up to and including `;`.
    fn query(&mut self, cmd: &'static str) -> Result<String, CatError> {
        self.send(cmd)?;
        let deadline = Instant::now() + self.timeout;
        let mut reply = Vec::new();
        let mut byte = [0u8; 1];
        while Instant::now() < deadline {
            match self.port.read(&mut byte) {
                Ok(1) => {
                    reply.push(byte[0]);
                    if byte[0] == b';' {
                        let text = String::from_utf8_lossy(&reply).trim().to_string();
                        if text == "?;" {
                            return Err(CatError::BadReply(format!("radio rejected {cmd}")));
                        }
                        return Ok(text);
                    }
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(CatError::NoReply(cmd))
    }

    /// Asks the radio for its ID; used to confirm we found the right port.
    pub fn identify(&mut self) -> Result<String, CatError> {
        let r = self.query("ID;")?;
        if r.starts_with("ID") {
            Ok(r)
        } else {
            Err(CatError::BadReply(r))
        }
    }
}

impl<P: Read + Write + Send> Rig for YaesuCat<P> {
    fn name(&self) -> &str {
        "Yaesu CAT"
    }

    fn set_data_mode(&mut self) -> Result<(), CatError> {
        // MD0C = main receiver, DATA-U
        self.send("MD0C;")
    }

    fn set_frequency(&mut self, hz: u64) -> Result<(), CatError> {
        self.send(&format!("FA{hz:09};"))
    }

    fn frequency(&mut self) -> Result<u64, CatError> {
        let r = self.query("FA;")?;
        r.trim_start_matches("FA").trim_end_matches(';').parse().map_err(|_| CatError::BadReply(r.clone()))
    }

    fn set_ptt(&mut self, transmit: bool) -> Result<(), CatError> {
        self.send(if transmit { "TX1;" } else { "TX0;" })?;
        self.keyed = transmit;
        Ok(())
    }
}

impl<P: Read + Write + Send> Drop for YaesuCat<P> {
    fn drop(&mut self) {
        if self.keyed {
            let _ = self.send("TX0;");
        }
    }
}

/// Serial ports that could be a radio. On macOS only the `/dev/cu.*` call-out devices are listed.
pub fn list_serial_ports() -> Vec<String> {
    let mut ports: Vec<String> = serialport::available_ports()
        .map(|v| v.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        ports.retain(|p| p.starts_with("/dev/cu."));
        ports.retain(|p| !p.contains("Bluetooth") && !p.contains("debug-console"));
    }
    ports.sort();
    ports
}

/// Opens a Yaesu radio on a serial port and checks that it answers.
pub fn open_yaesu(port_name: &str, baud: u32) -> Result<YaesuCat<Box<dyn serialport::SerialPort>>, CatError> {
    let mut port = serialport::new(port_name, baud)
        .timeout(Duration::from_millis(50))
        .open()
        .map_err(|e| CatError::Serial(e.to_string()))?;
    // Keep RTS/DTR low: some Yaesu menus use them for PTT or CW keying.
    // (Errors here are harmless; not every driver supports it.)
    let _ = port.write_request_to_send(false);
    let _ = port.write_data_terminal_ready(false);
    let mut cat = YaesuCat::new(port);
    cat.identify()?;
    Ok(cat)
}

/// Tries each port at each common baud rate and returns the first that answers `ID;`.
pub fn find_yaesu() -> Option<(String, u32)> {
    for port in list_serial_ports() {
        for baud in [38_400, 9_600, 4_800, 19_200] {
            if open_yaesu(&port, baud).is_ok() {
                return Some((port, baud));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A fake radio: records what we send and replies to queries.
    struct FakeRadio {
        sent: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        replies: VecDeque<u8>,
        freq: u64,
    }

    impl Read for FakeRadio {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.replies.pop_front() {
                Some(b) => {
                    buf[0] = b;
                    Ok(1)
                }
                None => Err(std::io::ErrorKind::TimedOut.into()),
            }
        }
    }

    impl Write for FakeRadio {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.sent.lock().unwrap().extend_from_slice(buf);
            let text = String::from_utf8_lossy(buf).to_string();
            if text == "ID;" {
                self.replies.extend(b"ID0800;");
            } else if text == "FA;" {
                self.replies.extend(format!("FA{:09};", self.freq).bytes());
            } else if let Some(f) = text.strip_prefix("FA").and_then(|t| t.strip_suffix(';')) {
                self.freq = f.parse().unwrap();
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn fake() -> FakeRadio {
        FakeRadio { sent: Default::default(), replies: VecDeque::new(), freq: 7_074_000 }
    }

    #[test]
    fn sends_the_right_yaesu_commands() {
        let radio = fake();
        let log = radio.sent.clone();
        let mut cat = YaesuCat::new(radio);
        assert_eq!(cat.identify().unwrap(), "ID0800;");
        cat.set_data_mode().unwrap();
        cat.set_frequency(14_074_000).unwrap();
        assert_eq!(cat.frequency().unwrap(), 14_074_000);
        cat.set_ptt(true).unwrap();
        cat.set_ptt(false).unwrap();
        let sent = String::from_utf8(log.lock().unwrap().clone()).unwrap();
        assert_eq!(sent, "ID;MD0C;FA014074000;FA;TX1;TX0;");
    }

    #[test]
    fn unkeys_the_radio_when_dropped() {
        let radio = fake();
        let log = radio.sent.clone();
        let mut cat = YaesuCat::new(radio);
        cat.set_ptt(true).unwrap();
        drop(cat);
        assert_eq!(String::from_utf8(log.lock().unwrap().clone()).unwrap(), "TX1;TX0;");
    }

    #[test]
    fn reports_a_silent_radio() {
        struct Silent;
        impl Read for Silent {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::TimedOut.into())
            }
        }
        impl Write for Silent {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut cat = YaesuCat::new(Silent);
        cat.timeout = Duration::from_millis(30);
        assert!(matches!(cat.identify(), Err(CatError::NoReply(_))));
    }
}
