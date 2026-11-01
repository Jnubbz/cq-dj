// Thin C shim between ft8_lib and the Rust engine.
// The decode loop and callsign hash table are adapted from ft8_lib's demo/decode_ft8.c
// (MIT License, Copyright (c) 2018 Karlis Goba).

#include "cqdj_ft8.h"

#include <math.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

#include <common/monitor.h>
#include <ft8/constants.h>
#include <ft8/decode.h>
#include <ft8/encode.h>
#include <ft8/message.h>

#define MIN_SCORE 10
#define MAX_CANDIDATES 200
#define LDPC_ITERATIONS 25
#define MAX_DECODED 100
#define FREQ_OSR 2
#define TIME_OSR 2

// ---------------------------------------------------------------------------
// Callsign hash table (lets the decoder resolve <hashed> callsigns it has seen before)
// ---------------------------------------------------------------------------

#define CALLSIGN_HASHTABLE_SIZE 256

static struct
{
    char callsign[12]; // up to 11 characters + NUL
    uint32_t hash;     // 8 MSBs: age; 22 LSBs: hash value
} callsign_hashtable[CALLSIGN_HASHTABLE_SIZE];

void cqdj_ft8_reset_hashes(void)
{
    memset(callsign_hashtable, 0, sizeof(callsign_hashtable));
}

static void hashtable_age(uint8_t max_age)
{
    for (int i = 0; i < CALLSIGN_HASHTABLE_SIZE; ++i)
    {
        if (callsign_hashtable[i].callsign[0] == '\0')
            continue;
        uint8_t age = (uint8_t)(callsign_hashtable[i].hash >> 24);
        if (age > max_age)
        {
            callsign_hashtable[i].callsign[0] = '\0';
            callsign_hashtable[i].hash = 0;
        }
        else
        {
            callsign_hashtable[i].hash = (((uint32_t)age + 1u) << 24) | (callsign_hashtable[i].hash & 0x3FFFFFu);
        }
    }
}

static void hashtable_add(const char* callsign, uint32_t hash)
{
    uint16_t hash10 = (hash >> 12) & 0x3FFu;
    int idx = (hash10 * 23) % CALLSIGN_HASHTABLE_SIZE;
    for (int probes = 0; probes < CALLSIGN_HASHTABLE_SIZE; ++probes)
    {
        if (callsign_hashtable[idx].callsign[0] == '\0')
        {
            strncpy(callsign_hashtable[idx].callsign, callsign, 11);
            callsign_hashtable[idx].callsign[11] = '\0';
            callsign_hashtable[idx].hash = hash;
            return;
        }
        if (((callsign_hashtable[idx].hash & 0x3FFFFFu) == hash) && (0 == strcmp(callsign_hashtable[idx].callsign, callsign)))
        {
            callsign_hashtable[idx].hash &= 0x3FFFFFu; // reset age
            return;
        }
        idx = (idx + 1) % CALLSIGN_HASHTABLE_SIZE;
    }
    // Table full: drop it. Hashed calls are rare; this is not worth evicting for.
}

static bool hashtable_lookup(ftx_callsign_hash_type_t hash_type, uint32_t hash, char* callsign)
{
    uint8_t shift = (hash_type == FTX_CALLSIGN_HASH_10_BITS) ? 12 : (hash_type == FTX_CALLSIGN_HASH_12_BITS ? 10 : 0);
    uint16_t hash10 = (hash >> (12 - shift)) & 0x3FFu;
    int idx = (hash10 * 23) % CALLSIGN_HASHTABLE_SIZE;
    for (int probes = 0; probes < CALLSIGN_HASHTABLE_SIZE; ++probes)
    {
        if (callsign_hashtable[idx].callsign[0] == '\0')
            break;
        if (((callsign_hashtable[idx].hash & 0x3FFFFFu) >> shift) == hash)
        {
            strcpy(callsign, callsign_hashtable[idx].callsign);
            return true;
        }
        idx = (idx + 1) % CALLSIGN_HASHTABLE_SIZE;
    }
    callsign[0] = '\0';
    return false;
}

static ftx_callsign_hash_interface_t hash_if = {
    .lookup_hash = hashtable_lookup,
    .save_hash = hashtable_add,
};

// ---------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------

int cqdj_ft8_tones(const char* message, uint8_t tones[CQDJ_FT8_NUM_TONES])
{
    ftx_message_t msg;
    ftx_message_rc_t rc = ftx_message_encode(&msg, &hash_if, message);
    if (rc != FTX_MESSAGE_RC_OK)
        return (int)rc;
    ft8_encode(msg.payload, tones);
    return 0;
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

static int cmp_float(const void* a, const void* b)
{
    float fa = *(const float*)a, fb = *(const float*)b;
    return (fa > fb) - (fa < fb);
}

// Median waterfall level in dB, used as the noise floor for SNR estimates.
// Most bins in an FT8 passband are noise, so the median is a robust floor.
static float waterfall_noise_db(const ftx_waterfall_t* wf)
{
    int total = wf->num_blocks * wf->block_stride;
    if (total <= 0)
        return -120.0f;
    int step = total / 20000 + 1; // subsample for speed
    int n = (total + step - 1) / step;
    float* vals = (float*)malloc(sizeof(float) * n);
    if (!vals)
        return -120.0f;
    int k = 0;
    for (int i = 0; i < total && k < n; i += step)
        vals[k++] = WF_ELEM_MAG(wf->mag[i]);
    qsort(vals, k, sizeof(float), cmp_float);
    float median = vals[k / 2];
    free(vals);
    return median;
}

// Average power in the transmitted tone of each symbol, relative to the noise floor.
static float estimate_snr_db(const ftx_waterfall_t* wf, const ftx_candidate_t* cand, const uint8_t* tones, float noise_db)
{
    double noise_lin = pow(10.0, noise_db / 10.0);
    double sum = 0;
    int count = 0;
    for (int s = 0; s < FT8_NN; ++s)
    {
        int block = cand->time_offset + s;
        if (block < 0 || block >= wf->num_blocks)
            continue;
        int offset = block;
        offset = offset * wf->time_osr + cand->time_sub;
        offset = offset * wf->freq_osr + cand->freq_sub;
        offset = offset * wf->num_bins + cand->freq_offset + tones[s];
        sum += pow(10.0, WF_ELEM_MAG(wf->mag[offset]) / 10.0);
        ++count;
    }
    if (count == 0)
        return -30.0f;
    double sig = sum / count - noise_lin; // remove the noise that sits under the tone
    if (sig <= noise_lin * 1e-3)
        sig = noise_lin * 1e-3;
    return (float)(10.0 * log10(sig / noise_lin));
}

int cqdj_ft8_decode(const float* samples, int num_samples, int sample_rate, float f_min, float f_max,
                    cqdj_decode_t* out, int max_out)
{
    if (!samples || num_samples <= 0 || sample_rate <= 0 || !out || max_out <= 0)
        return -1;

    monitor_t mon;
    monitor_config_t cfg = {
        .f_min = f_min,
        .f_max = f_max,
        .sample_rate = sample_rate,
        .time_osr = TIME_OSR,
        .freq_osr = FREQ_OSR,
        .protocol = FTX_PROTOCOL_FT8,
    };
    monitor_init(&mon, &cfg);

    for (int pos = 0; pos + mon.block_size <= num_samples && mon.wf.num_blocks < mon.wf.max_blocks; pos += mon.block_size)
        monitor_process(&mon, samples + pos);

    const ftx_waterfall_t* wf = &mon.wf;
    float noise_db = waterfall_noise_db(wf);

    ftx_candidate_t* candidates = (ftx_candidate_t*)malloc(sizeof(ftx_candidate_t) * MAX_CANDIDATES);
    ftx_message_t* decoded = (ftx_message_t*)calloc(MAX_DECODED, sizeof(ftx_message_t));
    bool* used = (bool*)calloc(MAX_DECODED, sizeof(bool));
    if (!candidates || !decoded || !used)
    {
        free(candidates);
        free(decoded);
        free(used);
        monitor_free(&mon);
        return -2;
    }

    int num_candidates = ftx_find_candidates(wf, MAX_CANDIDATES, candidates, MIN_SCORE);
    int num_out = 0;
    int num_decoded = 0;

    for (int i = 0; i < num_candidates && num_out < max_out && num_decoded < MAX_DECODED; ++i)
    {
        const ftx_candidate_t* cand = &candidates[i];
        ftx_message_t message;
        ftx_decode_status_t status;
        if (!ftx_decode_candidate(wf, cand, LDPC_ITERATIONS, &message, &status))
            continue;

        // Skip duplicates (the same message found at a neighbouring time/frequency offset)
        int idx = message.hash % MAX_DECODED;
        bool duplicate = false;
        while (used[idx])
        {
            if (decoded[idx].hash == message.hash && 0 == memcmp(decoded[idx].payload, message.payload, sizeof(message.payload)))
            {
                duplicate = true;
                break;
            }
            idx = (idx + 1) % MAX_DECODED;
        }
        if (duplicate)
            continue;
        used[idx] = true;
        decoded[idx] = message;
        ++num_decoded;

        cqdj_decode_t* r = &out[num_out];
        memset(r, 0, sizeof(*r));
        ftx_message_offsets_t offsets;
        char text[FTX_MAX_MESSAGE_LENGTH + 1];
        memset(text, 0, sizeof(text));
        if (ftx_message_decode(&message, &hash_if, text, &offsets) != FTX_MESSAGE_RC_OK)
            continue;
        strncpy(r->text, text, CQDJ_MAX_TEXT - 1);

        uint8_t tones[FT8_NN];
        ft8_encode(message.payload, tones);

        r->freq_hz = (mon.min_bin + cand->freq_offset + (float)cand->freq_sub / wf->freq_osr) / mon.symbol_period;
        r->time_sec = (cand->time_offset + (float)cand->time_sub / wf->time_osr) * mon.symbol_period;
        r->snr_db = estimate_snr_db(wf, cand, tones, noise_db);
        r->score = cand->score;
        r->ldpc_errors = status.ldpc_errors;
        ++num_out;
    }

    free(candidates);
    free(decoded);
    free(used);
    monitor_free(&mon);
    hashtable_age(10);
    return num_out;
}
