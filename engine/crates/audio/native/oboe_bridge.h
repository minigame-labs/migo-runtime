#pragma once
#include <stddef.h>
#include <stdint.h>

// Private to the native SDK, including consumers that export public migo_*.
#define MIGO_OBOE_LOCAL __attribute__((visibility("hidden")))

#ifdef __cplusplus
#define MIGO_OBOE_NOEXCEPT noexcept
extern "C" {
#else
#define MIGO_OBOE_NOEXCEPT
#endif

typedef struct MigoOboeStream MigoOboeStream;
typedef struct {
    void *context;
    void (*render)(void *context, float *samples, size_t sample_count);
    void (*release)(void *context);
} MigoOboeCallbacks;

typedef struct {
    int32_t sample_rate;
    int32_t channels;
    int32_t frames_per_burst;
    int32_t buffer_capacity_frames;
    int32_t buffer_size_frames;
    int32_t audio_api;
} MigoOboeConfig;

// Consumes the callback context on every outcome, including invalid arguments.
// Only the returned opaque pointer crosses the C++ object boundary.
MIGO_OBOE_LOCAL int32_t migo_oboe_open(MigoOboeCallbacks callbacks, MigoOboeConfig *config,
                       MigoOboeStream **output) MIGO_OBOE_NOEXCEPT;
MIGO_OBOE_LOCAL int32_t migo_oboe_start(MigoOboeStream *stream) MIGO_OBOE_NOEXCEPT;
MIGO_OBOE_LOCAL int32_t migo_oboe_pause(MigoOboeStream *stream) MIGO_OBOE_NOEXCEPT;
MIGO_OBOE_LOCAL int32_t migo_oboe_error(const MigoOboeStream *stream) MIGO_OBOE_NOEXCEPT;
MIGO_OBOE_LOCAL void migo_oboe_close(MigoOboeStream *stream) MIGO_OBOE_NOEXCEPT;

#ifdef __cplusplus
}
#endif
#undef MIGO_OBOE_NOEXCEPT
#undef MIGO_OBOE_LOCAL
