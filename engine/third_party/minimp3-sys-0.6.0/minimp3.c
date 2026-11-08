#include <stdlib.h>
#ifdef MIGO_MINIMP3_TEST
static size_t migo_heap_calls, migo_live_allocations;
static void *migo_malloc(size_t n) { void *p = malloc(n); migo_heap_calls++; if (p) migo_live_allocations++; return p; }
static void *migo_realloc(void *p, size_t n) { int was_null = !p; void *q = realloc(p, n); migo_heap_calls++; if (q && was_null) migo_live_allocations++; return q; }
static void migo_free(void *p) { if (p) migo_live_allocations--; free(p); }
size_t migo_mp3_test_heap_calls(void) { return migo_heap_calls; }
size_t migo_mp3_test_live_allocations(void) { return migo_live_allocations; }
#define malloc migo_malloc
#define realloc migo_realloc
#define free migo_free
#endif
#define MINIMP3_NO_STDIO
#include <minimp3_ex.h>
#include "migo_presentation.h"
#ifdef MIGO_MINIMP3_TEST
static size_t migo_codec_calls;
__attribute__((no_instrument_function))
void __cyg_profile_func_enter(void *function, void *caller) {
    (void)caller;
    if (function == (void *)mp3dec_decode_frame) migo_codec_calls++;
}
__attribute__((no_instrument_function))
void __cyg_profile_func_exit(void *function, void *caller) { (void)function; (void)caller; }
size_t migo_mp3_test_codec_calls(void) { return migo_codec_calls; }
/* Correctness oracle only: the original unbounded reader is never a step. */
int migo_mp3_test_full_reader(const uint8_t *data, size_t size,
    mp3d_sample_t *output, size_t capacity, size_t *actual)
{
    mp3dec_ex_t reader;
    int status = mp3dec_ex_open_buf(&reader, data, size, MP3D_DO_NOT_SCAN);
    if (status) return status;
    *actual = mp3dec_ex_read(&reader, output, capacity);
    status = reader.last_error;
    mp3dec_ex_close(&reader);
    return status;
}
#endif

struct migo_mp3_presentation {
    mp3dec_ex_t state;
    int initialized;
};

migo_mp3_presentation *migo_mp3_presentation_new(void)
{
    migo_mp3_presentation *p = (migo_mp3_presentation *)malloc(sizeof(*p));
    if (p) memset(p, 0, sizeof(*p));
    return p;
}

void migo_mp3_presentation_free(migo_mp3_presentation *p) { free(p); }

static int migo_mp3_presentation_begin_mode(migo_mp3_presentation *p, const uint8_t *frame,
    int size, int free_format_bytes, const mp3dec_frame_info_t *info,
    uint64_t limit, uint64_t *expected, int streaming)
{
    if (!p || p->initialized || !frame || size < HDR_SIZE || size > MAX_FREE_FORMAT_FRAME_SIZE || !info || !expected ||
        info->channels < 1 || info->channels > 2 || info->hz <= 0 ||
        info->layer < 1 || info->layer > 3 ||
        free_format_bytes < 0 || free_format_bytes > MAX_FREE_FORMAT_FRAME_SIZE || !hdr_valid(frame) ||
        hdr_frame_bytes(frame, free_format_bytes) + hdr_padding(frame) != size)
        return MP3D_E_PARAM;
    mp3dec_frame_info_t measured = *info;
    p->state.flags = MP3D_DO_NOT_SCAN;
    int status = mp3dec_load_index(&p->state, frame, size, free_format_bytes,
        (size_t)size, 0, &measured);
    p->initialized = 1;
    if ((status != MP3D_E_USER && status != 0) || p->state.index.frames ||
        (!streaming && p->state.detected_samples > MINIMP3_MIN(limit, UINT64_C(16777216))))
    {
        p->state.last_error = MP3D_E_DECODE;
        return MP3D_E_DECODE;
    }
    *expected = p->state.detected_samples;
    return p->state.start_offset == (uint64_t)size;
}

/* Whole-file and progressive callers share the same upstream metadata kernel.
 * Only whole-file PCM is subject to the contiguous result allocation ceiling. */
int migo_mp3_presentation_begin(migo_mp3_presentation *p, const uint8_t *frame,
    int size, int free_format_bytes, const mp3dec_frame_info_t *info,
    uint64_t limit, uint64_t *expected)
{
    return migo_mp3_presentation_begin_mode(p, frame, size, free_format_bytes,
        info, limit, expected, 0);
}

int migo_mp3_presentation_begin_stream(migo_mp3_presentation *p, const uint8_t *frame,
    int size, int free_format_bytes, const mp3dec_frame_info_t *info, uint64_t *expected)
{
    return migo_mp3_presentation_begin_mode(p, frame, size, free_format_bytes,
        info, 0, expected, 1);
}

int migo_mp3_presentation_decode(migo_mp3_presentation *p, mp3dec_t *decoder,
    const uint8_t *input, int size, mp3d_sample_t *pcm,
    mp3dec_frame_info_t *info, int *skip)
{
    if (!p || !p->initialized || p->state.last_error || !decoder || !input ||
        size < HDR_SIZE || size > 8192 || !pcm || !info || !skip)
        return MP3D_E_PARAM;
    mp3dec_ex_t *state = &p->state;
    int samples = mp3dec_decode_frame(decoder, input, size, pcm, info);
    *skip = 0;
    state->buffer_samples = samples;
    state->buffer_consumed = 0;
    if (!info->frame_bytes || !info->hz) return 0;
    if (info->frame_offset < 0 || info->frame_offset > size - HDR_SIZE ||
        info->frame_bytes > size ||
        mp3dec_ex_prepare_frame(state, input + info->frame_offset, info))
    {
        state->last_error = MP3D_E_DECODE;
        return MP3D_E_DECODE;
    }
    *skip = state->buffer_consumed;
    if (state->detected_samples && state->cur_sample >= state->detected_samples)
        return 0;
    return (int)mp3dec_ex_take_frame(state, MINIMP3_MAX_SAMPLES_PER_FRAME);
}

int migo_mp3_presentation_finish(const migo_mp3_presentation *p)
{
    if (!p || !p->initialized || p->state.last_error ||
        (p->state.detected_samples && p->state.cur_sample != p->state.detected_samples))
        return MP3D_E_DECODE;
    return 0;
}
