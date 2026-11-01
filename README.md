# CQ DJ

A simpler, modern take on multi-reply FT8: work several stations at once without fighting the software.
Pick stations yourself, or let **DJ** pick for you.

> Status: early development. The FT8 engine works and is tested; radio control and the app UI are next.

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
cargo test --release
```

The tests check:

- every standard QSO message (CQ, CQ POTA, grid, report, R-report, RR73, 73) decodes back to the same text
- five simultaneous signals mix without clipping and all decode at the right frequencies
- three slots still decode through noise at about −10 dB SNR
- SNR estimates land within 3 dB of the true value from −18 to 0 dB
- tones match ft8_lib's reference generator for the same message
- overlapping or out-of-band slots are refused

CI runs these on Linux and macOS for every push.

## Roadmap

1. ~~FT8 engine: encode, multi-signal TX audio, decode~~ ✔
2. Radio: audio devices, Hamlib CAT and PTT for the FT-710, live decodes, NTP clock check
3. Run mode, one slot: full auto-sequenced QSO
4. Multi-slot Run (2–5)
5. DJ: I pick / DJ picks, first come first served
6. Logging: ADIF + QRZ logbook
7. DX, Grid and POTA hunting; Hunt style
8. World Radio League upload; Windows build

## Credits

FT8 encoding and decoding use [ft8_lib](https://github.com/kgoba/ft8_lib) by Kārlis Goba (MIT License).
FT8 itself was designed by Joe Taylor, K1JT, and Steve Franke, K9AN.
