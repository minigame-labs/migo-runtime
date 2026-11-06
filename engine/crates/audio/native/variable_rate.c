/* SDK-private capacity query, compiled beside the pinned native filter types. */
#include "src_sinc.c"

size_t migo_src_required_bytes(int channels)
{
    if (channels < 1 || channels > 32) return 0;
    int length = 3 * (int)lrint((ARRAY_LEN(slow_high_qual_coeffs.coeffs) * 1.0)
                              / slow_high_qual_coeffs.increment * SRC_MAX_RATIO + 1);
    length = MAX(length, 4096) * channels + 1;
    return sizeof(SRC_STATE) + sizeof(SINC_FILTER)
           + sizeof(float) * (size_t)(length + channels);
}

size_t migo_src_history_frames(void)
{
    /* playbackRate 0.5..2.0 needs at most the 2x downsampling left window. */
    return (size_t)ceil((ARRAY_LEN(slow_high_qual_coeffs.coeffs) * 1.0)
                       / slow_high_qual_coeffs.increment * 2.0) + 2;
}
