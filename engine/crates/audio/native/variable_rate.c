/* SDK-private history query, compiled beside the pinned native filter types. */
#include "src_sinc.c"

size_t migo_src_history_frames(void)
{
    /* playbackRate 0.5..2.0 needs at most the 2x downsampling left window. */
    return (size_t)ceil((ARRAY_LEN(fastest_coeffs.coeffs) * 1.0)
                       / fastest_coeffs.increment * 2.0) + 2;
}
