//! Logging: QSO records, ADIF export, WSJT-X style ALL.TXT lines, and QRZ Logbook API replies.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

/// One completed contact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Qso {
    pub call: String,
    #[serde(default)]
    pub grid: String,
    /// Dial frequency + transmit audio offset, in Hz.
    pub freq_hz: u64,
    /// Start and end, Unix seconds (UTC).
    pub time_on: i64,
    pub time_off: i64,
    /// Reports like "-09" / "+02".
    #[serde(default)]
    pub rst_sent: String,
    #[serde(default)]
    pub rst_rcvd: String,
    pub my_call: String,
    #[serde(default)]
    pub my_grid: String,
    #[serde(default)]
    pub tx_power_w: Option<u32>,
    #[serde(default)]
    pub comment: String,
}

/// Amateur band name (ADIF style) for a frequency in Hz.
pub fn band_for_hz(hz: u64) -> Option<&'static str> {
    const BANDS: [(u64, u64, &str); 13] = [
        (1_800_000, 2_000_000, "160m"),
        (3_500_000, 4_000_000, "80m"),
        (5_060_000, 5_450_000, "60m"),
        (7_000_000, 7_300_000, "40m"),
        (10_100_000, 10_150_000, "30m"),
        (14_000_000, 14_350_000, "20m"),
        (18_068_000, 18_168_000, "17m"),
        (21_000_000, 21_450_000, "15m"),
        (24_890_000, 24_990_000, "12m"),
        (28_000_000, 29_700_000, "10m"),
        (50_000_000, 54_000_000, "6m"),
        (144_000_000, 148_000_000, "2m"),
        (420_000_000, 450_000_000, "70cm"),
    ];
    BANDS.iter().find(|(lo, hi, _)| hz >= *lo && hz <= *hi).map(|(_, _, b)| *b)
}

/// UTC (year, month, day, hour, minute, second) for Unix seconds. (Howard Hinnant's civil-from-days.)
pub fn utc_parts(t: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, (secs / 3600) as u32, ((secs % 3600) / 60) as u32, (secs % 60) as u32)
}

fn adif_field(out: &mut String, name: &str, value: &str) {
    if !value.is_empty() {
        out.push_str(&format!("<{}:{}>{} ", name, value.len(), value));
    }
}

impl Qso {
    /// One ADIF record, ending in `<EOR>`.
    pub fn to_adif(&self) -> String {
        let (y, mo, d, h, mi, s) = utc_parts(self.time_on);
        let (_, _, d2, h2, mi2, s2) = utc_parts(self.time_off);
        let (y2, mo2, _, _, _, _) = utc_parts(self.time_off);
        let mut r = String::new();
        adif_field(&mut r, "CALL", &self.call.to_ascii_uppercase());
        adif_field(&mut r, "GRIDSQUARE", &self.grid.to_ascii_uppercase());
        adif_field(&mut r, "MODE", "FT8");
        adif_field(&mut r, "RST_SENT", &self.rst_sent);
        adif_field(&mut r, "RST_RCVD", &self.rst_rcvd);
        adif_field(&mut r, "QSO_DATE", &format!("{y:04}{mo:02}{d:02}"));
        adif_field(&mut r, "TIME_ON", &format!("{h:02}{mi:02}{s:02}"));
        adif_field(&mut r, "QSO_DATE_OFF", &format!("{y2:04}{mo2:02}{d2:02}"));
        adif_field(&mut r, "TIME_OFF", &format!("{h2:02}{mi2:02}{s2:02}"));
        adif_field(&mut r, "BAND", band_for_hz(self.freq_hz).unwrap_or(""));
        adif_field(&mut r, "FREQ", &format!("{:.6}", self.freq_hz as f64 / 1e6));
        adif_field(&mut r, "STATION_CALLSIGN", &self.my_call.to_ascii_uppercase());
        adif_field(&mut r, "MY_GRIDSQUARE", &self.my_grid.to_ascii_uppercase());
        if let Some(p) = self.tx_power_w {
            adif_field(&mut r, "TX_PWR", &p.to_string());
        }
        adif_field(&mut r, "COMMENT", &self.comment);
        r.push_str("<EOR>");
        r
    }
}

/// Appends a QSO to an ADIF file, writing the header first if the file is new.
pub fn append_adif(path: &Path, qso: &Qso, program_version: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let new = !path.exists() || std::fs::metadata(path)?.len() == 0;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    if new {
        let mut h = String::from("CQ DJ log\n");
        adif_field(&mut h, "ADIF_VER", "3.1.4");
        adif_field(&mut h, "PROGRAMID", "CQ DJ");
        adif_field(&mut h, "PROGRAMVERSION", program_version);
        h.push_str("<EOH>\n");
        f.write_all(h.as_bytes())?;
    }
    writeln!(f, "{}", qso.to_adif())
}

/// A received line in WSJT-X's ALL.TXT format:
/// `190611_061900    14.074 Rx FT8    -17  0.1 2393 S92HP K1HTV -11`
pub fn all_txt_rx(slot_start: i64, dial_hz: u64, snr: i32, dt: f32, freq: f32, text: &str) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(slot_start);
    let dt = if dt.abs() < 0.05 { 0.0 } else { dt }; // print "0.0", never "-0.0"
    format!("{:02}{:02}{:02}_{:02}{:02}{:02} {:9.3} Rx FT8 {:6} {:4.1} {:4} {}", y % 100, mo, d, h, mi, s, dial_hz as f64 / 1e6, snr, dt, freq.round() as i32, text)
}

/// A transmitted line in the same format (SNR and DT are 0 for our own transmissions).
pub fn all_txt_tx(slot_start: i64, dial_hz: u64, freq: f32, text: &str) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(slot_start);
    format!("{:02}{:02}{:02}_{:02}{:02}{:02} {:9.3} Tx FT8 {:6} {:4.1} {:4} {}", y % 100, mo, d, h, mi, s, dial_hz as f64 / 1e6, 0, 0.0, freq.round() as i32, text)
}

pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

/// Result of a QRZ Logbook API call. QRZ replies with `KEY=value&KEY=value`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QrzReply {
    pub ok: bool,
    pub duplicate: bool,
    pub reason: String,
    pub logid: Option<String>,
    pub fields: Vec<(String, String)>,
}

pub fn parse_qrz_reply(body: &str) -> QrzReply {
    let fields: Vec<(String, String)> = body
        .trim()
        .split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            Some((k.trim().to_ascii_uppercase(), percent_decode(v.trim())))
        })
        .collect();
    let get = |k: &str| fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.clone());
    let result = get("RESULT").unwrap_or_default();
    let reason = get("REASON").unwrap_or_default();
    let duplicate = reason.to_ascii_lowercase().contains("duplicate");
    QrzReply { ok: result.eq_ignore_ascii_case("OK") || result.eq_ignore_ascii_case("REPLACE"), duplicate, reason, logid: get("LOGID"), fields }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else if b[i] == b'%' && i + 2 < b.len() {
            match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(v) => {
                    out.push(v);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Reads the callsigns (and band) already in an ADIF log, for "worked before" checks.
pub fn worked_from_adif(text: &str) -> Vec<(String, String)> {
    let upper = text.to_ascii_uppercase();
    let body = upper.split("<EOH>").last().unwrap_or("");
    body.split("<EOR>")
        .filter_map(|rec| {
            let call = adif_value(rec, "CALL")?;
            let band = adif_value(rec, "BAND").unwrap_or_default().to_ascii_lowercase();
            Some((call, band))
        })
        .collect()
}

fn adif_value(rec: &str, name: &str) -> Option<String> {
    let tag = format!("<{name}:");
    let start = rec.find(&tag)? + tag.len();
    let rest = &rec[start..];
    let close = rest.find('>')?;
    let len: usize = rest[..close].split(':').next()?.parse().ok()?;
    let value_start = close + 1;
    rest.get(value_start..value_start + len).map(|v| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qso() -> Qso {
        Qso {
            call: "ja1qrx".into(),
            grid: "PM95".into(),
            freq_hz: 14_074_000 + 1500,
            time_on: 1_793_543_535,  // 2026-11-01 14:32:15 UTC
            time_off: 1_793_543_595, // 14:33:15
            rst_sent: "-14".into(),
            rst_rcvd: "-09".into(),
            my_call: "KN0BLE".into(),
            my_grid: "EL98".into(),
            tx_power_w: Some(50),
            comment: String::new(),
        }
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc_parts(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(utc_parts(1_793_543_535), (2026, 11, 1, 14, 32, 15));
        assert_eq!(utc_parts(951_782_400), (2000, 2, 29, 0, 0, 0)); // leap day
    }

    #[test]
    fn adif_record() {
        let a = qso().to_adif();
        assert_eq!(
            a,
            "<CALL:6>JA1QRX <GRIDSQUARE:4>PM95 <MODE:3>FT8 <RST_SENT:3>-14 <RST_RCVD:3>-09 \
             <QSO_DATE:8>20261101 <TIME_ON:6>143215 <QSO_DATE_OFF:8>20261101 <TIME_OFF:6>143315 \
             <BAND:3>20m <FREQ:9>14.075500 <STATION_CALLSIGN:6>KN0BLE <MY_GRIDSQUARE:4>EL98 <TX_PWR:2>50 <EOR>"
        );
    }

    #[test]
    fn adif_file_and_worked_list() {
        let dir = std::env::temp_dir().join(format!("cqdj-test-{}", std::process::id()));
        let path = dir.join("log.adi");
        let _ = std::fs::remove_file(&path);
        append_adif(&path, &qso(), "0.1.0").unwrap();
        let mut q2 = qso();
        q2.call = "W7KZT".into();
        q2.freq_hz = 7_074_000;
        append_adif(&path, &q2, "0.1.0").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("<EOH>").count(), 1);
        assert_eq!(worked_from_adif(&text), vec![("JA1QRX".into(), "20m".into()), ("W7KZT".into(), "40m".into())]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn all_txt_matches_wsjtx_layout() {
        // WSJT-X: "190611_061900    14.074 Rx FT8    -17  0.1 2393 S92HP K1HTV -11"
        let t = 1_560_233_940; // 2019-06-11 06:19:00 UTC
        assert_eq!(all_txt_rx(t, 14_074_000, -17, 0.1, 2393.0, "S92HP K1HTV -11"), "190611_061900    14.074 Rx FT8    -17  0.1 2393 S92HP K1HTV -11");
        assert_eq!(all_txt_tx(t, 7_074_000, 1500.0, "CQ KN0BLE EL98"), "190611_061900     7.074 Tx FT8      0  0.0 1500 CQ KN0BLE EL98");
    }

    #[test]
    fn bands() {
        assert_eq!(band_for_hz(14_075_500), Some("20m"));
        assert_eq!(band_for_hz(50_314_000), Some("6m"));
        assert_eq!(band_for_hz(13_000_000), None);
    }

    #[test]
    fn qrz_replies() {
        let ok = parse_qrz_reply("RESULT=OK&LOGID=130877825&COUNT=1");
        assert!(ok.ok && !ok.duplicate);
        assert_eq!(ok.logid.as_deref(), Some("130877825"));
        let dup = parse_qrz_reply("RESULT=FAIL&REASON=Unable to add QSO to database: duplicate&EXTENDED=");
        assert!(!dup.ok && dup.duplicate);
        let bad = parse_qrz_reply("RESULT=AUTH&REASON=invalid api key xxxx");
        assert!(!bad.ok && !bad.duplicate);
        assert_eq!(bad.reason, "invalid api key xxxx");
        assert_eq!(parse_qrz_reply("RESULT=FAIL&REASON=a%20b+c").reason, "a b c");
    }
}
