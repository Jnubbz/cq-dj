// Thin C shim between ft8_lib and the Rust engine.
// Keeps the Rust FFI surface small: pack a message into tones, and decode a slot of audio.
#ifndef CQDJ_FT8_H
#define CQDJ_FT8_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define CQDJ_FT8_NUM_TONES 79
#define CQDJ_MAX_TEXT 36

typedef struct
{
    char text[CQDJ_MAX_TEXT]; // decoded message, NUL-terminated
    float freq_hz;            // audio frequency of tone 0
    float time_sec;           // start time offset from the start of the buffer
    float snr_db;             // estimated SNR in a 2500 Hz reference bandwidth (raw, before calibration)
    int score;                // Costas sync score from the candidate search
    int ldpc_errors;          // remaining LDPC parity errors (0 for a clean decode)
} cqdj_decode_t;

// Pack a text message (e.g. "CQ KN0BLE EL98") into 79 FT8 tones (values 0..7).
// Returns 0 on success, otherwise the ft8_lib ftx_message_rc_t error code.
int cqdj_ft8_tones(const char* message, uint8_t tones[CQDJ_FT8_NUM_TONES]);

// Decode one FT8 slot of mono float audio (nominally 15 s, but shorter is fine).
// Writes up to max_out results and returns how many were written (or a negative error).
// Not thread-safe: the callsign hash table is shared. The Rust wrapper serializes calls.
int cqdj_ft8_decode(const float* samples, int num_samples, int sample_rate, float f_min, float f_max,
                    cqdj_decode_t* out, int max_out);

// Forget callsigns learned from earlier decodes (used for hashed/nonstandard calls).
void cqdj_ft8_reset_hashes(void);

#ifdef __cplusplus
}
#endif

#endif
