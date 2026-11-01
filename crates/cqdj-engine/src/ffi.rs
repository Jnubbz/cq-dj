//! Raw bindings to `csrc/cqdj_ft8.h`. Use the safe wrappers in the rest of the crate instead.

use std::os::raw::{c_char, c_float, c_int};

pub const NUM_TONES: usize = 79;
pub const MAX_TEXT: usize = 36;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CqdjDecode {
    pub text: [c_char; MAX_TEXT],
    pub freq_hz: c_float,
    pub time_sec: c_float,
    pub snr_db: c_float,
    pub score: c_int,
    pub ldpc_errors: c_int,
}

impl Default for CqdjDecode {
    fn default() -> Self {
        CqdjDecode { text: [0; MAX_TEXT], freq_hz: 0.0, time_sec: 0.0, snr_db: 0.0, score: 0, ldpc_errors: 0 }
    }
}

extern "C" {
    pub fn cqdj_ft8_tones(message: *const c_char, tones: *mut u8) -> c_int;
    pub fn cqdj_ft8_decode(
        samples: *const c_float,
        num_samples: c_int,
        sample_rate: c_int,
        f_min: c_float,
        f_max: c_float,
        out: *mut CqdjDecode,
        max_out: c_int,
    ) -> c_int;
    pub fn cqdj_ft8_reset_hashes();
}
