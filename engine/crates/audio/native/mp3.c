/* Upstream minimp3 + minimp3_ex (engine/third_party/minimp3-afb604c, unmodified).
 *
 * The product MP3 source (src/mp3_source.rs) drives the frame decoder itself
 * through the migo_mp3_frame_* calls and minimp3_ex's own static helpers at
 * the end of this file, so it starts and seeks without minimp3_ex's whole
 * input buffer or whole-stream index. The mp3dec_ex calls (migo_mp3_open...)
 * remain as that source's test oracle (src/mp3_ex_oracle.rs).
 *
 * Every public minimp3 symbol is renamed so this copy can share an image with
 * any other minimp3 a dependency links. */
#define MINIMP3_NO_STDIO
#define MINIMP3_IMPLEMENTATION
#define mp3dec_init migo_minimp3_init
#define mp3dec_decode_frame migo_minimp3_decode_frame
#define mp3dec_f32_to_s16 migo_minimp3_f32_to_s16
#define mp3dec_detect_buf migo_minimp3_detect_buf
#define mp3dec_detect_cb migo_minimp3_detect_cb
#define mp3dec_load_buf migo_minimp3_load_buf
#define mp3dec_load_cb migo_minimp3_load_cb
#define mp3dec_iterate_buf migo_minimp3_iterate_buf
#define mp3dec_iterate_cb migo_minimp3_iterate_cb
#define mp3dec_ex_open_buf migo_minimp3_ex_open_buf
#define mp3dec_ex_open_cb migo_minimp3_ex_open_cb
#define mp3dec_ex_close migo_minimp3_ex_close
#define mp3dec_ex_seek migo_minimp3_ex_seek
#define mp3dec_ex_read_frame migo_minimp3_ex_read_frame
#define mp3dec_ex_read migo_minimp3_ex_read
#include "minimp3_ex.h"

#include <stdint.h>
#include <stdlib.h>

void *migo_mp3_open(mp3dec_io_t *io, int flags, int *error)
{
    mp3dec_ex_t *dec = (mp3dec_ex_t *)calloc(1, sizeof(mp3dec_ex_t));
    if (!dec)
    {
        *error = MP3D_E_MEMORY;
        return NULL;
    }
    int result = mp3dec_ex_open_cb(dec, io, flags);
    if (result)
    {
        mp3dec_ex_close(dec);
        free(dec);
        *error = result;
        return NULL;
    }
    *error = 0;
    return dec;
}

void migo_mp3_close(void *dec)
{
    if (dec)
    {
        mp3dec_ex_close((mp3dec_ex_t *)dec);
        free(dec);
    }
}

/* Format and the tagged (LAME/Xing/VBRI) length, interleaved samples. */
void migo_mp3_info(const void *dec, int *hz, int *channels, uint64_t *samples, int *tagged)
{
    const mp3dec_ex_t *d = (const mp3dec_ex_t *)dec;
    *hz = d->info.hz;
    *channels = d->info.channels;
    *samples = d->samples;
    *tagged = d->vbr_tag_found;
}

/* `position` counts interleaved samples, encoder delay excluded. */
int migo_mp3_seek(void *dec, uint64_t position)
{
    return mp3dec_ex_seek((mp3dec_ex_t *)dec, position);
}

size_t migo_mp3_read(void *dec, int16_t *buffer, size_t samples)
{
    return mp3dec_ex_read((mp3dec_ex_t *)dec, buffer, samples);
}

int migo_mp3_last_error(const void *dec)
{
    return ((const mp3dec_ex_t *)dec)->last_error;
}

/* ---- The frame decoder and minimp3_ex's own static helpers, for the pull
 * source in mp3_source.rs: it keeps the input window, the frame index and
 * the trimming itself, and calls exactly what mp3dec_ex calls for each step,
 * so its PCM and seek positions are mp3dec_ex's. */

void *migo_mp3_frame_new(void)
{
    mp3dec_t *dec = (mp3dec_t *)calloc(1, sizeof(mp3dec_t));
    if (dec)
        mp3dec_init(dec);
    return dec;
}

void migo_mp3_frame_free(void *dec)
{
    free(dec);
}

void migo_mp3_frame_reset(void *dec)
{
    mp3dec_init((mp3dec_t *)dec);
}

/* Decode (or, with a NULL `pcm`, only locate) the next frame in `buf`. */
int migo_mp3_frame_decode(void *dec, const uint8_t *buf, int len, int16_t *pcm, mp3dec_frame_info_t *info)
{
    return mp3dec_decode_frame((mp3dec_t *)dec, buf, len, pcm, info);
}

/* The next frame as mp3dec_iterate_cb finds it: its offset in `buf`, and
 * its size (0 when none is complete in `len` bytes). */
int migo_mp3_find_frame(const uint8_t *buf, int len, int *frame_size)
{
    int free_format_bytes = 0;
    *frame_size = 0;
    return mp3d_find_frame(buf, len, &free_format_bytes, frame_size);
}

int migo_mp3_frame_samples(const uint8_t *hdr)
{
    return (int)hdr_frame_samples(hdr);
}

int migo_mp3_frame_layer(const uint8_t *hdr)
{
    return 4 - HDR_GET_LAYER(hdr);
}

int migo_mp3_frame_channels(const uint8_t *hdr)
{
    return HDR_IS_MONO(hdr) ? 1 : 2;
}

int migo_mp3_frame_hz(const uint8_t *hdr)
{
    return (int)hdr_sample_rate_hz(hdr);
}

/* mp3dec_check_vbrtag: 1 with frames/delay/padding for a Xing/Info (LAME)
 * frame, -1 for a tag without a frame count, 0 for an audio frame. */
int migo_mp3_vbr_tag(const uint8_t *frame, int size, uint32_t *frames, int *delay, int *padding)
{
    return mp3dec_check_vbrtag(frame, size, frames, delay, padding);
}

size_t migo_mp3_id3v2_size(const uint8_t *buf, size_t len)
{
    return mp3dec_skip_id3v2(buf, len);
}

/* Drop a trailing ID3v1 / APE tag from the last `*len` bytes of a stream. */
void migo_mp3_trim_id3v1(const uint8_t *buf, size_t *len)
{
    mp3dec_skip_id3v1(buf, len);
}

/* Main-data bytes a Layer III frame holds after its side info (what a seek
 * counts toward the bit reservoir), or -1 when the side info is corrupt. */
int migo_mp3_main_data_bytes(const uint8_t *frame, int size)
{
    bs_t bs[1];
    L3_gr_info_t gr_info[4];
    bs_init(bs, frame + HDR_SIZE, size - HDR_SIZE);
    if (HDR_IS_CRC(frame))
        get_bits(bs, 16);
    if (L3_read_side_info(bs, gr_info, frame) < 0)
        return -1;
    return (bs->limit - bs->pos) / 8;
}
