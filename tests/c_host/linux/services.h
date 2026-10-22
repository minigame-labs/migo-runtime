/*
 * The device services this host supplies through the host-service channel
 * (migo/host_services.h): permission, location, the motion sensors and a
 * camera -- one that makes its own pictures, a gradient that moves, and
 * photographs (BMP) and records (YUV4MPEG2) them -- and a microphone that hears
 * a steady 440 Hz tone, recorded as WAV or raw PCM with its frames -- and video
 * players that play any source as a clip two seconds long, with nothing on
 * screen.
 *
 * The host runs on a desktop -- often a VM -- with no motion sensors and no
 * position of its own, so its device is scripted, as its gamepad and keyboard
 * are (main.c). The script is a feed: a file named by MIGO_C_HOST_DEVICE_FEED
 * that whoever runs the host appends lines to while it runs,
 *
 *   SET-SENSOR acceleration <x>:<y>:<z>            m/s^2, gravity included
 *   SET-SENSOR gyroscope <x>:<y>:<z>               rad/s
 *   SET-SENSOR orientation <azimuth>:<pitch>:<roll>  degrees; the heading is used
 *   SET-LOCATION <latitude> <longitude>            WGS-84 degrees
 *
 * -- the Android emulator console's sensor names and units, which are what
 * migo-conformance's bundles ask for. Until a line says otherwise the device
 * lies flat and face up, facing north, at rest, with no position fix.
 *
 * The engine calls services_on_call on its own thread, and nothing is done
 * there: the call is queued, and services_pump, run from the host's loop,
 * answers it, reads the feed and delivers the readings that are due. A host
 * that did its work on the engine's thread would be doing it on a thread it
 * never agreed to own (main.c's keyboard follows the same rule).
 */
#ifndef MIGO_C_HOST_SERVICES_H_
#define MIGO_C_HOST_SERVICES_H_

#include <migo/migo.h>

/* What the host declares in MigoHostCallbacks.host_services. */
#define C_HOST_SERVICES                                                        \
    ((UINT64_C(1) << MIGO_HOST_SERVICE_PERMISSION) |                           \
     (UINT64_C(1) << MIGO_HOST_SERVICE_LOCATION) |                             \
     (UINT64_C(1) << MIGO_HOST_SERVICE_MOTION) |                               \
     (UINT64_C(1) << MIGO_HOST_SERVICE_CAMERA) |                               \
     (UINT64_C(1) << MIGO_HOST_SERVICE_RECORDER) |                             \
     (UINT64_C(1) << MIGO_HOST_SERVICE_VIDEO))

/* Opens the feed, when MIGO_C_HOST_DEVICE_FEED names one. */
void services_init(void);

/* MigoHostCallbacks.on_host_service_call. */
void MIGO_CALL services_on_call(void *user_data, MigoSession *session,
                                const MigoHostServiceCall *call);

/* From the host's loop, every tick, while a Surface is attached. */
void services_pump(MigoSession *session);

void services_shutdown(void);

#endif /* MIGO_C_HOST_SERVICES_H_ */
