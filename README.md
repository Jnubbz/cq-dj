# CQ DJ

A simpler, modern take on multi-reply FT8: work several stations at once without fighting the software.
Pick stations yourself, or let **DJ** pick for you.

> Status: early development. You can decode, call CQ and work stations by hand on a Yaesu FT-710.
> Auto-sequencing, multiple slots and DJ come next.

## Trying it on the radio (FT-710 + Mac)

### Install
1. Download the `.dmg` from the latest [release](https://github.com/Jnubbz/cq-dj/releases/latest).
2. Open it and drag **CQ DJ** to Applications.
3. The app isn't signed by Apple yet, so macOS will block it the first time. In Terminal, run:
   ```
   xattr -dr com.apple.quarantine "/Applications/CQ DJ.app"
   ```
   then open it normally. When macOS asks for microphone access, click **Allow**: that's how CQ DJ hears the radio.
   If it never asks and you get no audio, run `tccutil reset Microphone com.kn0ble.cqdj` and open CQ DJ again.

You only do this once. Every merge to `main` publishes a new signed release, and CQ DJ offers
**Install and restart** when one is available. You can also use **Check for updates** in settings.

### First run
1. **Quit WSJT-X**, since only one program can use the radio's port at a time.
2. In setup, enter your callsign and grid. Pick **USB Audio CODEC** (the radio) for both sound card fields.
3. For radio control, click **Find my radio**, or pick the same port and speed you use in WSJT-X.
   The radio settings that already work for WSJT-X are all CQ DJ needs.
4. Click **Start radio**. CQ DJ sets the radio to DATA-U on the band you pick and checks your clock against a time server.
   If the clock is off, it corrects for it automatically.

### Test checklist
- [ ] **Receive:** within a minute, decodes fill **Band activity**. Keep the **Receive level** needle in the green zone.
- [ ] **Tune into a dummy load, or at low power:** press **Tune** for a steady tone. Raise **Transmit audio level** until power
      stops rising, then back off a little. **ALC must read zero.** Press **Stop tune** (it also stops by itself after 10 s).
- [ ] **Call CQ:** press **Call CQ**. The radio keys at the start of your slot and sends `CQ <you> <grid>`. Check
      PSK Reporter to see where you're heard.
- [ ] **Work someone by hand:** click a station in Band activity (or Calling you). CQ DJ switches to the opposite slot and
      offers the standard messages. The next one is outlined after each reply. Click it to send.
- [ ] **Stop TX** unkeys immediately. Closing the window also unkeys.

Safety: the transmitter is never keyed for longer than 14.5 s per slot. A watchdog unkeys it if anything goes wrong.

## What it will do

- **Run mode with multiple slots.** Call CQ and work 1–5 stations at once, each on its own audio frequency.
- **DJ picks, or you do.** A toggle switches between hand-picking callers and letting DJ choose the next one:
  - **First come, first served** (default). Ties go to whoever has called the most cycles in a row, then the stronger signal.
  - **DX hunting**, **Grid hunting** and **POTA hunting** (parks come from pota.app spots).
- **Plain language.** It tells you what it did and why ("Skipped K4MPX, already worked on 20m").
- **Logging** to ADIF and your QRZ logbook (API key). World Radio League later.
- **Desktop app** for macOS and Windows (Tauri). First target: Yaesu FT-710 over one USB cable.

### About power with multiple slots

Each extra simultaneous signal shares the transmitter's peak power, so each stream gets weaker by
20·log10(N): **2 slots −6 dB, 3 slots −9.5 dB, 4 slots −12 dB, 5 slots −14 dB** per signal
(same figures K1JT and K9AN give for WSJT-X Fox mode). Keep ALC at zero; multi-signal audio needs a linear transmitter.

## Repository layout

```
crates/cqdj-engine/      FT8 engine (Rust)
  vendor/ft8_lib/        ft8_lib by Kārlis Goba (MIT), vendored library sources
  csrc/                  small C shim between ft8_lib and Rust
  src/lib.rs             encode, GFSK synthesis, multi-signal mixing, decode
  tests/roundtrip.rs     end-to-end tests (encode → audio → noise → decode)
crates/cqdj-radio/       radio layer
  src/audio.rs           sound card in/out (cpal), 12 kHz receive buffer
  src/cat.rs             Yaesu CAT (MD0C, FA, TX1/TX0, ID) over USB serial
  src/clock.rs           FT8 slot timing, SNTP clock check and correction
  src/station.rs         the station loop: decode each slot, key and transmit on time
  tests/station_timing.rs  real-time loop test with a fake radio and sound card
app/                     Tauri desktop app (Rust shell + plain HTML/JS UI)
```

## Engine API (so far)

```rust
use cqdj_engine::*;

// Three QSOs at once, mixed into one transmit stream
let audio = build_tx_audio(&[
    TxSignal::new("JA1QRX KN0BLE -14", 800.0),
    TxSignal::new("EA8DJX KN0BLE RR73", 1300.0),
    TxSignal::new("W7KZT KN0BLE EL98", 1900.0),
], SAMPLE_RATE, 1.0)?;

// Decode a 15-second receive slot (12 kHz mono f32, starting at the slot boundary)
for d in decode(&slot, SAMPLE_RATE, DecodeOptions::default())? {
    println!("{:+3} dB {:+.1} s {:4.0} Hz  {}", d.snr_db, d.dt_sec, d.freq_hz, d.text);
}
```

## Building and testing

You need Rust (stable) and a C compiler (Xcode command line tools on macOS).

```
cargo test --release                       # engine + radio layer
cargo test --release -- --include-ignored  # also the 45 s real-time station test
cd app && npm ci && npx tauri build        # the desktop app
```

The tests check:

- every standard QSO message (CQ, CQ POTA, grid, report, R-report, RR73, 73) decodes back to the same text
- five simultaneous signals mix without clipping and all decode at the right frequencies
- three slots still decode through noise at about −10 dB SNR
- SNR estimates land within 3 dB of the true value from −18 to 0 dB
- tones match ft8_lib's reference generator for the same message
- overlapping or out-of-band slots are refused

- the station loop keys PTT at +0.2 s, starts audio at +0.5 s and unkeys at about +13.2 s, decodes
  the other slot, and never decodes its own transmission
- Yaesu CAT commands, receive resampling (48/44.1/96 kHz → 12 kHz) and NTP parsing

CI runs these on Linux and macOS for every push, and builds the Mac app.

## Releases

`.github/workflows/release.yml` runs on every push to `main`. It tests, builds the Mac app, and signs the
update with the Tauri updater key. Then it publishes release `v0.1.<run number>` with a `latest.json` that the app checks.
It needs two repository secrets: `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
The matching public key is in `app/src-tauri/tauri.conf.json`. Updates signed with any other key are refused.

## Roadmap

1. ~~FT8 engine: encode, multi-signal TX audio, decode~~ ✔
2. ~~Radio: audio devices, CAT and PTT for the FT-710, live decodes, clock check, manual QSOs~~ ✔ (testing)
3. Run mode, one slot: full auto-sequenced QSO
4. Multi-slot Run (2–5)
5. DJ: I pick / DJ picks, first come first served
6. Logging: ADIF + QRZ logbook
7. DX, Grid and POTA hunting; Hunt style
8. World Radio League upload; Windows build

## Credits

FT8 encoding and decoding use [ft8_lib](https://github.com/kgoba/ft8_lib) by Kārlis Goba (MIT License).
FT8 itself was designed by Joe Taylor, K1JT, and Steve Franke, K9AN.
