/* Upstream minimp3 + minimp3_ex (engine/third_party/minimp3-afb604c, unmodified),
 * compiled here so the streaming decoder can use mp3dec_ex's pull reader:
 * callback IO, LAME/Xing gapless trimming and sample-exact seeking.
 *
 * Every public minimp3 symbol is renamed so this copy can share an image with
 * any other minimp3 a dependency links. Rust sees only the migo_mp3_* calls
 * below; mp3dec_ex_t stays opaque to it. */
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
