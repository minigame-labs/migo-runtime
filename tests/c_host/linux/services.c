/*
 * The device services this host supplies: see services.h.
 */

/* open, read and pthreads are POSIX, not ISO C11; mkstemps is the C library's. */
#define _POSIX_C_SOURCE 200809L
#define _DEFAULT_SOURCE

#include "services.h"

#include <fcntl.h>
#include <math.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static const double STANDARD_GRAVITY = 9.80665;
static const double PI = 3.14159265358979323846;

/* ---- The device: touched only from the host's loop. ---- */

static struct {
    double acceleration[3]; /* m/s^2, gravity included, in the ABI's axes */
    double gyroscope[3];    /* rad/s */
    double heading;         /* degrees clockwise from north, [0, 360) */
    int has_fix;
    double latitude;
    double longitude;
} g_device = {{0.0, 0.0, 9.80665}, {0.0, 0.0, 0.0}, 0.0, 0, 0.0, 0.0};

/* A started sensor delivers a reading every interval_ms, on a schedule rather
 * than "interval after the last one": the loop ticks every 16 ms, and "game"
 * asks for 20, which only a schedule meets on average. */
typedef struct Sensor {
    MigoSensorKind kind;
    int running;
    long interval_ms;
    long due_ms;
} Sensor;

/* In MIGO_MOTION_* order: a method's sensor is method / 2, and an even method
 * starts it. */
static Sensor g_sensors[4] = {
    {MIGO_SENSOR_ACCELEROMETER, 0, 0, 0},
    {MIGO_SENSOR_GYROSCOPE, 0, 0, 0},
    {MIGO_SENSOR_COMPASS, 0, 0, 0},
    {MIGO_SENSOR_DEVICE_MOTION, 0, 0, 0},
};

/* The compass takes no interval; the reference's heading arrives five times a
 * second. */
static const long COMPASS_INTERVAL_MS = 200;

static long monotonic_ms(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (long)now.tv_sec * 1000L + now.tv_nsec / 1000000L;
}

/* ---- Calls, queued by the engine's thread for the host's loop. ---- */

/* A call as the loop needs it. The payload is the engine's only for the
 * callback, so what the loop needs from it is read there. */
typedef struct QueuedCall {
    uint64_t call_id;
    MigoHostService service;
    uint32_t method;
    long arg;      /* a START's interval in ms; a REQUEST_SCOPE's MigoScope; a camera's
                      or a recording's id */
    double number; /* SET_ZOOM's zoom */
    struct {
        long duration_ms;
        long sample_rate;
        long channels;
        long frame_bytes; /* 0: no frames */
        int format;       /* RecordFormat */
    } record; /* a recorder START's options */
} QueuedCall;

/* What this host records into: a WAV file, or raw 16-bit PCM. The compressed
 * formats need an encoder this host does not have. */
typedef enum { RECORD_WAV, RECORD_PCM, RECORD_UNSUPPORTED } RecordFormat;

#define QUEUE_CAPACITY 64U
static QueuedCall g_queue[QUEUE_CAPACITY];
static unsigned g_queue_read;
static unsigned g_queue_written;
static pthread_mutex_t g_queue_lock = PTHREAD_MUTEX_INITIALIZER;

static int queue_push(const QueuedCall *call) {
    pthread_mutex_lock(&g_queue_lock);
    int pushed = g_queue_written - g_queue_read < QUEUE_CAPACITY;
    if (pushed) g_queue[g_queue_written++ % QUEUE_CAPACITY] = *call;
    pthread_mutex_unlock(&g_queue_lock);
    return pushed;
}

static int queue_pop(QueuedCall *call) {
    pthread_mutex_lock(&g_queue_lock);
    int popped = g_queue_read != g_queue_written;
    if (popped) *call = g_queue[g_queue_read++ % QUEUE_CAPACITY];
    pthread_mutex_unlock(&g_queue_lock);
    return popped;
}

static void complete(MigoSession *session, uint64_t call_id, const char *payload_json,
                     const char *failure) {
    MigoHostServiceResult result;
    memset(&result, 0, sizeof(result));
    result.struct_size = (uint32_t)sizeof(result);
    result.abi_version = MIGO_ABI_VERSION_CURRENT;
    if (failure != NULL) {
        result.status = MIGO_HOST_SERVICE_STATUS_FAIL;
        result.message_utf8 = failure;
        result.message_length = (uint32_t)strlen(failure);
    } else {
        result.status = MIGO_HOST_SERVICE_STATUS_OK;
        result.payload_json_utf8 = payload_json;
        result.payload_length = (uint32_t)strlen(payload_json);
    }
    MigoResult r = migo_session_complete_host_service_call(session, call_id, &result);
    if (r != MIGO_OK) {
        fprintf(stderr, "[c-host] completing a host-service call failed: %d\n", (int)r);
        fflush(stderr);
    }
}

/* The value of a top-level "key" in a JSON object of `length` bytes (not
 * NUL-terminated): where it begins, or NULL. Enough for the engine's own small
 * payloads; a key inside a string value is escaped there and never matches. */
static const char *json_value(const char *json, size_t length, const char *key) {
    const char *end = json + length;
    size_t key_length = strlen(key);
    for (const char *p = json; p + key_length + 2 <= end; p++) {
        if (p[0] != '"' || memcmp(p + 1, key, key_length) != 0 || p[key_length + 1] != '"') {
            continue;
        }
        const char *q = p + key_length + 2;
        while (q < end && (*q == ' ' || *q == '\t' || *q == '\n' || *q == '\r')) q++;
        if (q == end || *q != ':') continue;
        q++;
        while (q < end && (*q == ' ' || *q == '\t' || *q == '\n' || *q == '\r')) q++;
        return q < end ? q : NULL;
    }
    return NULL;
}

static int value_is(const char *value, const char *end, const char *literal) {
    size_t n = strlen(literal);
    return value != NULL && (size_t)(end - value) >= n && memcmp(value, literal, n) == 0;
}

/* A START's {"interval"}: "game" 20 ms, "ui" 60, "normal" -- the default -- 200. */
static long start_interval(const char *json, size_t length, uint32_t method) {
    if (method == MIGO_MOTION_START_COMPASS) return COMPASS_INTERVAL_MS;
    const char *end = json + length;
    const char *value = json_value(json, length, "interval");
    if (value_is(value, end, "\"game\"")) return 20;
    if (value_is(value, end, "\"ui\"")) return 60;
    return 200;
}

void MIGO_CALL services_on_call(void *user_data, MigoSession *session,
                                const MigoHostServiceCall *call) {
    (void)user_data;
    QueuedCall queued;
    memset(&queued, 0, sizeof(queued));
    queued.call_id = call->call_id;
    queued.service = call->service;
    queued.method = call->method;
    const char *json = call->payload_json_utf8 != NULL ? call->payload_json_utf8 : "";
    size_t length = call->payload_json_utf8 != NULL ? call->payload_length : 0;
    if (call->service == MIGO_HOST_SERVICE_MOTION && call->method % 2 == 0) {
        queued.arg = start_interval(json, length, call->method);
    } else if (call->service == MIGO_HOST_SERVICE_PERMISSION) {
        const char *value = json_value(json, length, "scopeId");
        queued.arg = value != NULL ? strtol(value, NULL, 10) : -1;
    } else if (call->service == MIGO_HOST_SERVICE_CAMERA) {
        const char *value = json_value(json, length, "cameraId");
        queued.arg = value != NULL ? strtol(value, NULL, 10) : -1;
        const char *zoom = json_value(json, length, "zoom");
        queued.number = zoom != NULL ? strtod(zoom, NULL) : 1.0;
    } else if (call->service == MIGO_HOST_SERVICE_RECORDER) {
        const char *end = json + length;
        const char *value = json_value(json, length, "recordingId");
        queued.arg = value != NULL ? strtol(value, NULL, 10) : 0;
        value = json_value(json, length, "duration");
        queued.record.duration_ms = value != NULL ? strtol(value, NULL, 10) : 60000;
        value = json_value(json, length, "sampleRate");
        queued.record.sample_rate = value != NULL ? strtol(value, NULL, 10) : 8000;
        value = json_value(json, length, "numberOfChannels");
        queued.record.channels = value != NULL ? strtol(value, NULL, 10) : 2;
        value = json_value(json, length, "frameSize");
        queued.record.frame_bytes = value != NULL ? strtol(value, NULL, 10) * 1024 : 0;
        value = json_value(json, length, "format");
        queued.record.format = value_is(value, end, "\"wav\"")   ? RECORD_WAV
                               : value_is(value, end, "\"PCM\"") ? RECORD_PCM
                                                                 : RECORD_UNSUPPORTED;
    }
    if (!queue_push(&queued)) {
        /* A full queue is the loop not keeping up; refuse rather than wait on
         * the engine's thread. A command has no one to tell. */
        if (call->call_id != 0) complete(session, call->call_id, NULL, "the host is busy");
        fprintf(stderr, "[c-host] host-service queue full: service %u method %u dropped\n",
                (unsigned)call->service, (unsigned)call->method);
        fflush(stderr);
    }
}

/* ---- Answering calls on the host's loop. ---- */

/* This host runs what its developer put in it -- the examples and
 * migo-conformance -- so it grants every scope content asks for, as a host that
 * trusts what it runs would. A host running other people's content asks its
 * user, and records the answer the same way. */
static void answer_permission(MigoSession *session, const QueuedCall *call) {
    if (call->method != MIGO_PERMISSION_REQUEST_SCOPE || call->arg < 0) {
        complete(session, call->call_id, NULL, "unknown request");
        return;
    }
    MigoResult r = migo_session_set_scope_state(session, (MigoScope)call->arg,
                                                 MIGO_SCOPE_STATE_GRANTED);
    if (r != MIGO_OK) {
        complete(session, call->call_id, NULL, "unknown scope");
        return;
    }
    complete(session, call->call_id, "", NULL);
}

/* Answered in WGS-84; the engine converts to GCJ-02 when content asked for it
 * and rounds the fuzzy position itself. A scripted fix is exact. */
static void answer_location(MigoSession *session, const QueuedCall *call) {
    if (!g_device.has_fix) {
        complete(session, call->call_id, NULL, "no position fix");
        return;
    }
    char json[256];
    if (call->method == MIGO_LOCATION_GET_LOCATION) {
        snprintf(json, sizeof(json),
                 "{\"latitude\":%.10g,\"longitude\":%.10g,\"speed\":0,\"accuracy\":0,"
                 "\"altitude\":0,\"verticalAccuracy\":0,\"horizontalAccuracy\":0}",
                 g_device.latitude, g_device.longitude);
    } else if (call->method == MIGO_LOCATION_GET_FUZZY_LOCATION) {
        snprintf(json, sizeof(json), "{\"latitude\":%.10g,\"longitude\":%.10g}",
                 g_device.latitude, g_device.longitude);
    } else {
        complete(session, call->call_id, NULL, "unknown request");
        return;
    }
    complete(session, call->call_id, json, NULL);
}

/* A START answers once the sensor runs, and its first reading is due at once;
 * a STOP is a command, and nothing is delivered after it. */
static void answer_motion(MigoSession *session, const QueuedCall *call, long now) {
    if (call->method > MIGO_MOTION_STOP_DEVICE_MOTION) {
        if (call->call_id != 0) complete(session, call->call_id, NULL, "unknown request");
        return;
    }
    Sensor *sensor = &g_sensors[call->method / 2];
    if (call->method % 2 == 0) {
        sensor->running = 1;
        sensor->interval_ms = call->arg;
        sensor->due_ms = now;
        complete(session, call->call_id, "", NULL);
    } else {
        sensor->running = 0;
    }
}

/* ---- The camera: the device's one, which makes its own pictures. ---- */

#define CAMERA_WIDTH 64
#define CAMERA_HEIGHT 48
#define CAMERA_FRAME_MS 33L

static struct {
    int open;
    long id;
    int listening;
    int recording;
    long next_frame_ms;
    uint32_t frame;          /* the picture it shows now */
    uint32_t recording_from; /* the first picture of the recording */
} g_camera;

/* Picture n, BGRA: blue across, green down, red along the diagonal and moved on
 * by its number -- every channel a picture, every picture its own. */
static void camera_picture(uint32_t n, uint8_t bgra[CAMERA_HEIGHT][CAMERA_WIDTH][4]) {
    for (int y = 0; y < CAMERA_HEIGHT; y++) {
        for (int x = 0; x < CAMERA_WIDTH; x++) {
            bgra[y][x][0] = (uint8_t)(x * 4);
            bgra[y][x][1] = (uint8_t)(y * 5);
            bgra[y][x][2] = (uint8_t)((x + y) * 2 + n);
            bgra[y][x][3] = 255;
        }
    }
}

/* A new file of the host's own in the system's temporary directory, named for
 * what it holds: the engine takes it as the call completes. NULL on failure. */
static FILE *staged(const char *suffix, char *path, size_t size) {
    snprintf(path, size, "/tmp/migo-c-host-XXXXXX%s", suffix);
    int fd = mkstemps(path, (int)strlen(suffix));
    if (fd < 0) return NULL;
    FILE *file = fdopen(fd, "wb");
    if (file == NULL) close(fd);
    return file;
}

static void put_u16(FILE *file, unsigned value) {
    fputc((int)(value & 0xFF), file);
    fputc((int)((value >> 8) & 0xFF), file);
}

static void put_u32(FILE *file, unsigned long value) {
    put_u16(file, (unsigned)(value & 0xFFFF));
    put_u16(file, (unsigned)((value >> 16) & 0xFFFF));
}

/* Picture n as a 24-bit BMP, which content decodes, into `path`. */
static int write_picture(uint32_t n, char *path, size_t size) {
    FILE *file = staged(".bmp", path, size);
    if (file == NULL) return 0;
    static uint8_t bgra[CAMERA_HEIGHT][CAMERA_WIDTH][4];
    camera_picture(n, bgra);
    const unsigned long pixels = CAMERA_WIDTH * CAMERA_HEIGHT * 3UL;
    fputs("BM", file);
    put_u32(file, 54UL + pixels);
    put_u32(file, 0);
    put_u32(file, 54);
    put_u32(file, 40);
    put_u32(file, CAMERA_WIDTH);
    put_u32(file, CAMERA_HEIGHT);
    put_u16(file, 1);
    put_u16(file, 24);
    put_u32(file, 0);
    put_u32(file, pixels);
    put_u32(file, 2835);
    put_u32(file, 2835);
    put_u32(file, 0);
    put_u32(file, 0);
    /* Bottom row first; 64 x 3 bytes needs no padding to four. */
    for (int y = CAMERA_HEIGHT - 1; y >= 0; y--) {
        for (int x = 0; x < CAMERA_WIDTH; x++) fwrite(bgra[y][x], 1, 3, file);
    }
    return fclose(file) == 0;
}

/* Pictures first .. first + count - 1 as a YUV4MPEG2 video at thirty a second:
 * the plainest video there is, which GStreamer and FFmpeg read. */
static int write_video(uint32_t first, uint32_t count, char *path, size_t size) {
    FILE *file = staged(".y4m", path, size);
    if (file == NULL) return 0;
    static uint8_t bgra[CAMERA_HEIGHT][CAMERA_WIDTH][4];
    static uint8_t luma[CAMERA_HEIGHT][CAMERA_WIDTH];
    static uint8_t cb[CAMERA_HEIGHT / 2][CAMERA_WIDTH / 2];
    static uint8_t cr[CAMERA_HEIGHT / 2][CAMERA_WIDTH / 2];
    fprintf(file, "YUV4MPEG2 W%d H%d F30:1 Ip A1:1 C420jpeg\n", CAMERA_WIDTH, CAMERA_HEIGHT);
    for (uint32_t n = first; n < first + count; n++) {
        camera_picture(n, bgra);
        /* Full-range BT.601, chroma of each 2 x 2 block's top left. */
        for (int y = 0; y < CAMERA_HEIGHT; y++) {
            for (int x = 0; x < CAMERA_WIDTH; x++) {
                double b = bgra[y][x][0], g = bgra[y][x][1], r = bgra[y][x][2];
                luma[y][x] = (uint8_t)lround(0.299 * r + 0.587 * g + 0.114 * b);
                if (x % 2 == 0 && y % 2 == 0) {
                    cb[y / 2][x / 2] = (uint8_t)lround(128 - 0.168736 * r - 0.331264 * g + 0.5 * b);
                    cr[y / 2][x / 2] = (uint8_t)lround(128 + 0.5 * r - 0.418688 * g - 0.081312 * b);
                }
            }
        }
        fputs("FRAME\n", file);
        fwrite(luma, 1, sizeof(luma), file);
        fwrite(cb, 1, sizeof(cb), file);
        fwrite(cr, 1, sizeof(cr), file);
    }
    return fclose(file) == 0;
}

static void answer_camera(MigoSession *session, const QueuedCall *call, long now) {
    char json[1024];
    char picture[256];
    char video[256];
    int open = g_camera.open && g_camera.id == call->arg;
    switch (call->method) {
    case MIGO_CAMERA_CREATE:
        g_camera.open = 1;
        g_camera.id = call->arg;
        g_camera.listening = 0;
        g_camera.recording = 0;
        g_camera.next_frame_ms = now;
        complete(session, call->call_id, "{}", NULL);
        return;
    case MIGO_CAMERA_DESTROY:
        if (open) memset(&g_camera, 0, sizeof(g_camera));
        return;
    case MIGO_CAMERA_LISTEN_FRAME_CHANGE:
    case MIGO_CAMERA_CLOSE_FRAME_CHANGE:
        if (open) g_camera.listening = call->method == MIGO_CAMERA_LISTEN_FRAME_CHANGE;
        return;
    case MIGO_CAMERA_SET:
        /* Nothing on screen: where its preview stands changes nothing here. */
        return;
    default:
        break;
    }
    if (!open) {
        complete(session, call->call_id, NULL, "camera not open");
        return;
    }
    switch (call->method) {
    case MIGO_CAMERA_TAKE_PHOTO:
        if (!write_picture(g_camera.frame, picture, sizeof(picture))) {
            complete(session, call->call_id, NULL, "the photo could not be written");
            return;
        }
        snprintf(json, sizeof(json), "{\"tempImagePath\":\"%s\",\"width\":%d,\"height\":%d}",
                 picture, CAMERA_WIDTH, CAMERA_HEIGHT);
        complete(session, call->call_id, json, NULL);
        return;
    case MIGO_CAMERA_START_RECORD:
        if (g_camera.recording) {
            complete(session, call->call_id, NULL, "already recording");
            return;
        }
        g_camera.recording = 1;
        g_camera.recording_from = g_camera.frame;
        complete(session, call->call_id, "{}", NULL);
        return;
    case MIGO_CAMERA_STOP_RECORD: {
        if (!g_camera.recording) {
            complete(session, call->call_id, NULL, "not recording");
            return;
        }
        g_camera.recording = 0;
        uint32_t count = g_camera.frame - g_camera.recording_from + 1;
        if (!write_video(g_camera.recording_from, count, video, sizeof(video)) ||
            !write_picture(g_camera.recording_from, picture, sizeof(picture))) {
            complete(session, call->call_id, NULL, "the video could not be written");
            return;
        }
        snprintf(json, sizeof(json), "{\"tempVideoPath\":\"%s\",\"tempThumbPath\":\"%s\"}", video,
                 picture);
        complete(session, call->call_id, json, NULL);
        return;
    }
    case MIGO_CAMERA_SET_ZOOM: {
        /* A camera that zooms from 1 to 4, to a tenth. */
        double zoom = call->number < 1.0 ? 1.0 : call->number > 4.0 ? 4.0 : call->number;
        snprintf(json, sizeof(json), "{\"zoom\":%.1f}", zoom);
        complete(session, call->call_id, json, NULL);
        return;
    }
    default:
        if (call->call_id != 0) complete(session, call->call_id, NULL, "unknown request");
        return;
    }
}

/* The camera's next picture, due every frame; content is handed it while it
 * listens. */
static void camera_tick(MigoSession *session, long now) {
    if (!g_camera.open || now < g_camera.next_frame_ms) return;
    g_camera.frame++;
    g_camera.next_frame_ms += CAMERA_FRAME_MS;
    if (g_camera.next_frame_ms <= now) g_camera.next_frame_ms = now + CAMERA_FRAME_MS;
    if (!g_camera.listening) return;
    static uint8_t bgra[CAMERA_HEIGHT][CAMERA_WIDTH][4];
    camera_picture(g_camera.frame, bgra);
    MigoCameraFrame frame;
    memset(&frame, 0, sizeof(frame));
    frame.struct_size = (uint32_t)sizeof(frame);
    frame.abi_version = MIGO_ABI_VERSION_CURRENT;
    frame.camera_id = (uint32_t)g_camera.id;
    frame.width = CAMERA_WIDTH;
    frame.height = CAMERA_HEIGHT;
    frame.row_stride = CAMERA_WIDTH * 4;
    frame.format = MIGO_PIXEL_FORMAT_BGRA8;
    frame.pixels = &bgra[0][0][0];
    MigoResult r = migo_session_post_camera_frame(session, &frame);
    if (r != MIGO_OK && r != MIGO_ERROR_INVALID_STATE) {
        fprintf(stderr, "[c-host] a camera frame was refused: %d\n", (int)r);
        fflush(stderr);
    }
}

/* ---- The microphone: one that hears a steady tone. ---- */

static const double TONE_HZ = 440.0;

static struct {
    int active;
    int paused;
    long id;
    long sample_rate;
    long channels;
    long duration_ms;
    int format;
    FILE *file;
    char path[256];
    uint64_t frames_written; /* sample frames: one sample per channel */
    long resumed_ms;         /* when it last started or resumed recording */
    long recorded_ms;        /* recorded before that */
    uint8_t *frame;          /* the frame being filled, frame_bytes long */
    long frame_bytes;
    long frame_fill;
} g_recording;

static void post_recorder_event(MigoSession *session, uint32_t event, const char *json) {
    MigoResult r = migo_session_post_host_service_event(session, MIGO_HOST_SERVICE_RECORDER,
                                                        event, json, (uint32_t)strlen(json));
    if (r != MIGO_OK) {
        fprintf(stderr, "[c-host] a recorder event was refused: %d\n", (int)r);
        fflush(stderr);
    }
}

static void recorder_fail(MigoSession *session, long id, const char *reason) {
    char json[256];
    snprintf(json, sizeof(json), "{\"recordingId\":%ld,\"errMsg\":\"%s\"}", id, reason);
    post_recorder_event(session, MIGO_RECORDER_EVENT_ERROR, json);
}

static void post_recorder_frame(MigoSession *session, int last) {
    MigoRecorderFrame frame;
    memset(&frame, 0, sizeof(frame));
    frame.struct_size = (uint32_t)sizeof(frame);
    frame.abi_version = MIGO_ABI_VERSION_CURRENT;
    frame.recording_id = (uint32_t)g_recording.id;
    frame.is_last_frame = last ? 1U : 0U;
    frame.data = g_recording.frame_fill > 0 ? g_recording.frame : NULL;
    frame.length = (uint32_t)g_recording.frame_fill;
    MigoResult r = migo_session_post_recorder_frame(session, &frame);
    if (r != MIGO_OK && r != MIGO_ERROR_WOULD_BLOCK) {
        fprintf(stderr, "[c-host] a recorder frame was refused: %d\n", (int)r);
        fflush(stderr);
    }
    g_recording.frame_fill = 0;
}

/* A WAV header for `data_bytes` of 16-bit PCM, written at the file's start. */
static void wav_header(FILE *file, long sample_rate, long channels, unsigned long data_bytes) {
    fputs("RIFF", file);
    put_u32(file, 36UL + data_bytes);
    fputs("WAVEfmt ", file);
    put_u32(file, 16);
    put_u16(file, 1);
    put_u16(file, (unsigned)channels);
    put_u32(file, (unsigned long)sample_rate);
    put_u32(file, (unsigned long)(sample_rate * channels * 2));
    put_u16(file, (unsigned)(channels * 2));
    put_u16(file, 16);
    fputs("data", file);
    put_u32(file, data_bytes);
}

/* The tone from where the recording is up to until `now`, into the file and
 * the frames. */
static void record_until(MigoSession *session, long now) {
    if (!g_recording.active || g_recording.paused) return;
    long elapsed = g_recording.recorded_ms + (now - g_recording.resumed_ms);
    if (elapsed > g_recording.duration_ms) elapsed = g_recording.duration_ms;
    uint64_t due = (uint64_t)elapsed * (uint64_t)g_recording.sample_rate / 1000U;
    for (; g_recording.frames_written < due; g_recording.frames_written++) {
        double t = (double)g_recording.frames_written / (double)g_recording.sample_rate;
        int16_t sample = (int16_t)lround(16000.0 * sin(2.0 * PI * TONE_HZ * t));
        for (long c = 0; c < g_recording.channels; c++) {
            uint8_t bytes[2] = {(uint8_t)(sample & 0xFF), (uint8_t)((sample >> 8) & 0xFF)};
            fwrite(bytes, 1, 2, g_recording.file);
            if (g_recording.frame_bytes > 0) {
                for (int b = 0; b < 2; b++) {
                    g_recording.frame[g_recording.frame_fill++] = bytes[b];
                    if (g_recording.frame_fill == g_recording.frame_bytes) {
                        post_recorder_frame(session, 0);
                    }
                }
            }
        }
    }
}

/* The recording ends: the file finished, the last frame and STOP reported --
 * or, when `report` is 0 (a new START replaces it), nothing. */
static void recorder_finish(MigoSession *session, long now, int report) {
    if (!g_recording.active) return;
    record_until(session, now);
    long recorded = g_recording.paused
                        ? g_recording.recorded_ms
                        : g_recording.recorded_ms + (now - g_recording.resumed_ms);
    if (recorded > g_recording.duration_ms) recorded = g_recording.duration_ms;
    unsigned long data_bytes =
        (unsigned long)(g_recording.frames_written * (uint64_t)g_recording.channels * 2U);
    if (g_recording.format == RECORD_WAV) {
        fseek(g_recording.file, 0, SEEK_SET);
        wav_header(g_recording.file, g_recording.sample_rate, g_recording.channels, data_bytes);
    }
    long file_size = (long)data_bytes + (g_recording.format == RECORD_WAV ? 44 : 0);
    int written = fclose(g_recording.file) == 0;
    if (report && g_recording.frame_bytes > 0) post_recorder_frame(session, 1);
    if (report) {
        if (!written) {
            recorder_fail(session, g_recording.id, "the recording could not be written");
        } else {
            char json[512];
            snprintf(json, sizeof(json),
                     "{\"recordingId\":%ld,\"tempFilePath\":\"%s\",\"duration\":%ld,"
                     "\"fileSize\":%ld}",
                     g_recording.id, g_recording.path, recorded, file_size);
            post_recorder_event(session, MIGO_RECORDER_EVENT_STOP, json);
        }
    } else {
        remove(g_recording.path);
    }
    free(g_recording.frame);
    memset(&g_recording, 0, sizeof(g_recording));
}

static void answer_recorder(MigoSession *session, const QueuedCall *call, long now) {
    char json[64];
    snprintf(json, sizeof(json), "{\"recordingId\":%ld}", call->arg);
    int ours = g_recording.active && g_recording.id == call->arg;
    switch (call->method) {
    case MIGO_RECORDER_START: {
        recorder_finish(session, now, 0);
        if (call->record.format == RECORD_UNSUPPORTED) {
            recorder_fail(session, call->arg,
                          "recorderManager.start:fail this host records wav and PCM only");
            return;
        }
        if (call->record.sample_rate <= 0 || call->record.channels < 1 ||
            call->record.channels > 2) {
            recorder_fail(session, call->arg, "recorderManager.start:fail invalid audio parameters");
            return;
        }
        g_recording.file = staged(call->record.format == RECORD_WAV ? ".wav" : ".pcm",
                                  g_recording.path, sizeof(g_recording.path));
        if (g_recording.file == NULL) {
            recorder_fail(session, call->arg, "recorderManager.start:fail no file to record into");
            return;
        }
        if (call->record.format == RECORD_WAV) {
            wav_header(g_recording.file, call->record.sample_rate, call->record.channels, 0);
        }
        if (call->record.frame_bytes > 0) {
            g_recording.frame = malloc((size_t)call->record.frame_bytes);
            if (g_recording.frame == NULL) {
                fclose(g_recording.file);
                remove(g_recording.path);
                recorder_fail(session, call->arg, "recorderManager.start:fail out of memory");
                return;
            }
            g_recording.frame_bytes = call->record.frame_bytes;
        }
        g_recording.active = 1;
        g_recording.id = call->arg;
        g_recording.sample_rate = call->record.sample_rate;
        g_recording.channels = call->record.channels;
        g_recording.duration_ms = call->record.duration_ms;
        g_recording.format = call->record.format;
        g_recording.resumed_ms = now;
        post_recorder_event(session, MIGO_RECORDER_EVENT_START, json);
        return;
    }
    case MIGO_RECORDER_PAUSE:
        if (!ours || g_recording.paused) return;
        record_until(session, now);
        g_recording.recorded_ms += now - g_recording.resumed_ms;
        g_recording.paused = 1;
        post_recorder_event(session, MIGO_RECORDER_EVENT_PAUSE, json);
        return;
    case MIGO_RECORDER_RESUME:
        if (!ours || !g_recording.paused) return;
        g_recording.paused = 0;
        g_recording.resumed_ms = now;
        post_recorder_event(session, MIGO_RECORDER_EVENT_RESUME, json);
        return;
    case MIGO_RECORDER_STOP:
        if (ours) recorder_finish(session, now, 1);
        return;
    default:
        return;
    }
}

/* The tone recorded as time passes; the recording stops on its own when its
 * duration runs out. */
static void recorder_tick(MigoSession *session, long now) {
    if (!g_recording.active || g_recording.paused) return;
    if (g_recording.recorded_ms + (now - g_recording.resumed_ms) >= g_recording.duration_ms) {
        recorder_finish(session, now, 1);
    } else {
        record_until(session, now);
    }
}

/* ---- Readings. ---- */

static double wrap_degrees(double degrees) {
    double wrapped = fmod(degrees, 360.0);
    return wrapped < 0 ? wrapped + 360.0 : wrapped;
}

static double degrees(double radians) {
    return radians * (180.0 / PI);
}

/*
 * Device motion for a device at rest: its tilt is where gravity points, its
 * heading the compass's. The W3C DeviceOrientation angles of R = Rz(alpha)
 * Rx(beta) Ry(gamma), decomposed as Android's DeviceSensorManager does (the
 * specification's, Chromium's), from R's bottom row, (-cos(beta) sin(gamma),
 * sin(beta), cos(beta) cos(gamma)) -- the up direction in the device's axes,
 * which is what an accelerometer at rest reads. Alpha turns the other way from
 * a heading: a device facing east has alpha 270.
 */
static void device_motion(double out[3]) {
    const double *a = g_device.acceleration;
    double norm = sqrt(a[0] * a[0] + a[1] * a[1] + a[2] * a[2]);
    double r6 = 0.0, r7 = 0.0, r8 = 1.0;
    if (norm > 0.0) {
        r6 = a[0] / norm;
        r7 = a[1] / norm;
        r8 = a[2] / norm;
    }
    double s = r7 > 1.0 ? 1.0 : r7 < -1.0 ? -1.0 : r7;
    double beta, gamma;
    if (r8 > 0) {
        beta = asin(s);
        gamma = atan2(-r6, r8);
    } else if (r8 < 0) {
        beta = -asin(s);
        beta += beta >= 0 ? -PI : PI;
        gamma = atan2(r6, -r8);
    } else if (r6 > 0) {
        beta = asin(s);
        gamma = -PI / 2;
    } else if (r6 < 0) {
        beta = -asin(s);
        beta += beta >= 0 ? -PI : PI;
        gamma = -PI / 2;
    } else {
        beta = s > 0 ? PI / 2 : -PI / 2;
        gamma = 0;
    }
    out[0] = wrap_degrees(360.0 - g_device.heading);
    out[1] = degrees(beta);
    out[2] = degrees(gamma);
}

static void deliver(MigoSession *session, const Sensor *sensor) {
    MigoSensorSample sample;
    memset(&sample, 0, sizeof(sample));
    sample.struct_size = (uint32_t)sizeof(sample);
    sample.abi_version = MIGO_ABI_VERSION_CURRENT;
    sample.kind = sensor->kind;
    switch (sensor->kind) {
    case MIGO_SENSOR_ACCELEROMETER:
        for (int i = 0; i < 3; i++) sample.values[i] = g_device.acceleration[i] / STANDARD_GRAVITY;
        break;
    case MIGO_SENSOR_GYROSCOPE:
        for (int i = 0; i < 3; i++) sample.values[i] = g_device.gyroscope[i];
        break;
    case MIGO_SENSOR_COMPASS:
        sample.values[0] = g_device.heading;
        sample.compass_accuracy = MIGO_COMPASS_ACCURACY_HIGH;
        break;
    case MIGO_SENSOR_DEVICE_MOTION:
        device_motion(sample.values);
        break;
    default:
        return;
    }
    MigoResult r = migo_session_post_sensor_sample(session, &sample);
    if (r != MIGO_OK) {
        fprintf(stderr, "[c-host] a sensor reading was refused: %d\n", (int)r);
        fflush(stderr);
    }
}

/* ---- The feed. ---- */

static int g_feed = -1;
static char g_line[256];
static size_t g_line_length;
static int g_line_too_long;

static void apply(const char *line) {
    char name[32];
    double v[3];
    if (sscanf(line, "SET-SENSOR %31s %lf:%lf:%lf", name, &v[0], &v[1], &v[2]) == 4) {
        if (strcmp(name, "acceleration") == 0) {
            memcpy(g_device.acceleration, v, sizeof(v));
        } else if (strcmp(name, "gyroscope") == 0) {
            memcpy(g_device.gyroscope, v, sizeof(v));
        } else if (strcmp(name, "orientation") == 0) {
            g_device.heading = wrap_degrees(v[0]);
        } else {
            printf("[c-host] feed: this device has no %s sensor\n", name);
            fflush(stdout);
            return;
        }
    } else if (sscanf(line, "SET-LOCATION %lf %lf", &v[0], &v[1]) == 2) {
        g_device.has_fix = 1;
        g_device.latitude = v[0];
        g_device.longitude = v[1];
    } else {
        printf("[c-host] feed: not understood: %s\n", line);
        fflush(stdout);
        return;
    }
    printf("[c-host] feed: %s\n", line);
    fflush(stdout);
}

/* Whatever was appended since the last tick, a line at a time; a partial line
 * waits for the rest. */
static void read_feed(void) {
    if (g_feed < 0) return;
    char chunk[512];
    ssize_t n;
    while ((n = read(g_feed, chunk, sizeof(chunk))) > 0) {
        for (ssize_t i = 0; i < n; i++) {
            char c = chunk[i];
            if (c == '\n') {
                g_line[g_line_length] = '\0';
                if (g_line_too_long) {
                    fprintf(stderr, "[c-host] feed: a line longer than %zu bytes was dropped\n",
                            sizeof(g_line) - 1);
                } else if (g_line_length > 0) {
                    apply(g_line);
                }
                g_line_length = 0;
                g_line_too_long = 0;
            } else if (c != '\r') {
                if (g_line_length < sizeof(g_line) - 1) {
                    g_line[g_line_length++] = c;
                } else {
                    g_line_too_long = 1;
                }
            }
        }
    }
}

void services_init(void) {
    const char *path = getenv("MIGO_C_HOST_DEVICE_FEED");
    if (path == NULL || path[0] == '\0') return;
    g_feed = open(path, O_RDONLY | O_CLOEXEC);
    if (g_feed < 0) {
        fprintf(stderr, "[c-host] cannot open the device feed %s; the device stays as it is\n", path);
        fflush(stderr);
    }
}

void services_pump(MigoSession *session) {
    long now = monotonic_ms();
    read_feed();
    QueuedCall call;
    while (queue_pop(&call)) {
        switch (call.service) {
        case MIGO_HOST_SERVICE_PERMISSION:
            answer_permission(session, &call);
            break;
        case MIGO_HOST_SERVICE_LOCATION:
            answer_location(session, &call);
            break;
        case MIGO_HOST_SERVICE_MOTION:
            answer_motion(session, &call, now);
            break;
        case MIGO_HOST_SERVICE_CAMERA:
            answer_camera(session, &call, now);
            break;
        case MIGO_HOST_SERVICE_RECORDER:
            answer_recorder(session, &call, now);
            break;
        default:
            if (call.call_id != 0) complete(session, call.call_id, NULL, "unknown service");
            break;
        }
    }
    camera_tick(session, now);
    recorder_tick(session, now);
    for (size_t i = 0; i < sizeof(g_sensors) / sizeof(g_sensors[0]); i++) {
        Sensor *sensor = &g_sensors[i];
        if (!sensor->running || now < sensor->due_ms) continue;
        deliver(session, sensor);
        sensor->due_ms += sensor->interval_ms;
        /* After a stall, resume the schedule rather than burst to catch up. */
        if (sensor->due_ms <= now) sensor->due_ms = now + sensor->interval_ms;
    }
}

void services_shutdown(void) {
    if (g_feed >= 0) close(g_feed);
    g_feed = -1;
}
