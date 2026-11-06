/* Private bounded adapter to minimp3_ex; the caller measures a complete first
 * frame through minimp3 and owns the encoded buffer and mp3dec_t. No index/IO
 * reader is opened. begin calls the upstream metadata callback once; decode
 * calls the existing codec once, then the shared upstream trimming helpers.
 * All sample counts and skip offsets are interleaved, across every channel.
 * Caller limits input windows and yields/checks cancellation between calls.
 */
#ifndef MIGO_PRESENTATION_H
#define MIGO_PRESENTATION_H
#include "minimp3/minimp3.h"
#include <stdint.h>
typedef struct migo_mp3_presentation migo_mp3_presentation;
migo_mp3_presentation *migo_mp3_presentation_new(void);
void migo_mp3_presentation_free(migo_mp3_presentation *);
/* Returns 1 to skip the tag frame, 0 for ordinary PCM, or a negative error.
 * expected=0 retains the upstream unknown-duration path. No heap allocation. */
int migo_mp3_presentation_begin(migo_mp3_presentation *, const uint8_t *, int,
    int, const mp3dec_frame_info_t *, uint64_t, uint64_t *);
/* Progressive PCM has a bounded resident window, rather than a duration cap. */
int migo_mp3_presentation_begin_stream(migo_mp3_presentation *, const uint8_t *, int,
    int, const mp3dec_frame_info_t *, uint64_t *);
/* Returns interleaved samples in pcm[skip..skip+count], or a negative error.
 * The original minimp3 consumed-byte fields remain in info, including when
 * trimming consumes the whole frame. Does not publish partial-file success. */
int migo_mp3_presentation_decode(migo_mp3_presentation *, mp3dec_t *,
    const uint8_t *, int, mp3d_sample_t *, mp3dec_frame_info_t *, int *);
/* Reject a known-length stream that ended before its declared presentation. */
int migo_mp3_presentation_finish(const migo_mp3_presentation *);
#endif
