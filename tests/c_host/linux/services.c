/*
 * The device services this host supplies: see services.h.
 */

/* open, read and pthreads are POSIX, not ISO C11. */
#define _POSIX_C_SOURCE 200809L

#include "services.h"

#include <fcntl.h>
#include <math.h>
#include <pthread.h>
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
    long arg; /* a START's interval in ms; a REQUEST_SCOPE's MigoScope */
} QueuedCall;

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
    QueuedCall queued = {call->call_id, call->service, call->method, 0};
    const char *json = call->payload_json_utf8 != NULL ? call->payload_json_utf8 : "";
    size_t length = call->payload_json_utf8 != NULL ? call->payload_length : 0;
    if (call->service == MIGO_HOST_SERVICE_MOTION && call->method % 2 == 0) {
        queued.arg = start_interval(json, length, call->method);
    } else if (call->service == MIGO_HOST_SERVICE_PERMISSION) {
        const char *value = json_value(json, length, "scopeId");
        queued.arg = value != NULL ? strtol(value, NULL, 10) : -1;
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
        default:
            if (call.call_id != 0) complete(session, call.call_id, NULL, "unknown service");
            break;
        }
    }
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
