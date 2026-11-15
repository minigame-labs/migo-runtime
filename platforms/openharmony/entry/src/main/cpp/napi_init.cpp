/*
 * OpenHarmony native host for migo.
 *
 * The engine is reached only through the public C ABI under <migo/>; nothing
 * here includes an engine header. That is the same discipline the Android
 * NativeActivity host and the Linux/Win32 hosts follow, and it is what makes
 * this file a consumer of the SDK rather than part of it.
 *
 * The surface arrives from ArkUI's XComponent, whose OnSurfaceCreated callback
 * hands over an OHNativeWindow*. That pointer is exactly what
 * MigoOpenHarmonyNativeWindowDescriptor carries, so no translation is needed --
 * only ownership discipline: the host keeps its reference, the engine takes its
 * own, and the host must not destroy the window until the release observer
 * reports RELEASED.
 */

#include <ace/xcomponent/native_interface_xcomponent.h>
#include <hilog/log.h>
#include <napi/native_api.h>

#include <cmath>
#include <cstdint>
#include <cstring>
#include <memory>
#include <mutex>
#include <new>
#include <string>

#include <migo/migo.h>
/* migo.h is the engine/session umbrella and deliberately pulls in no platform
 * descriptor: including one would drag a platform SDK header into hosts that
 * have nothing to do with it. The typed descriptor is opted into here. */
#include <migo/platform/openharmony.h>

#define MIGO_LOG_TAG "migo-host"
#define LOGI(...)                                                              \
  OH_LOG_Print(LOG_APP, LOG_INFO, 0xF000, MIGO_LOG_TAG, __VA_ARGS__)
#define LOGE(...)                                                              \
  OH_LOG_Print(LOG_APP, LOG_ERROR, 0xF000, MIGO_LOG_TAG, __VA_ARGS__)

namespace {

enum class StartState : uint8_t {
  Stopped,
  Starting,
  Started,
  Stopping,
};

struct Host {
  /* State is read by the N-API/UI callback and the asynchronous release
   * callback. No engine call is made while this mutex is held. */
  std::mutex state_mutex;
  /* The C ABI permits different Sessions to be driven concurrently, but the
   * host must serialize calls through one Session. Native XComponent events
   * are not guaranteed to share ArkTS's thread, so this is broader than a
   * surface-only lock. It is never held while waiting for GPU completion. */
  std::mutex api_mutex;
  napi_threadsafe_function dispatcher = nullptr;
  MigoEngine *engine = nullptr;
  MigoSession *session = nullptr;
  MigoSurfaceAttachment *attachment = nullptr;
  MigoSurfaceRelease *release = nullptr;
  /* The surface can arrive before the engine exists: ArkUI creates it when
   * the component is laid out, while the engine is created from the page's
   * onLoad. Whichever happens second performs the attach, so neither
   * ordering loses the window. */
  OH_NativeXComponent *pending_component = nullptr;
  void *pending_window = nullptr;
  uint64_t generation = 0;
  uint64_t detaching_generation = 0;
  uint64_t release_generation = 0;
  uint64_t release_callback_generation = 0;
  bool release_callback_seen = false;
  bool release_completion_claimed = false;
  bool detach_in_progress = false;
  bool teardown_claimed = false;
  StartState start_state = StartState::Stopped;
  /* Desired Ability level plus the last level successfully delivered to the
   * live Session. Keeping both makes a transient ABI failure retryable. */
  bool foreground = true;
  bool applied_foreground = true;
  bool foreground_state_valid = false;
  bool content_loaded = false;
  std::string files_dir;
  std::string cache_dir;
  std::string content_id;
  /* Physical pixels per CSS pixel. Kept because touch coordinates cross the
   * ABI in CSS pixels while the platform reports physical ones. */
  float scale_factor = 1.0f;
};

Host g_host;
std::mutex g_dispatcher_init_mutex;
constexpr size_t kMaxPathBytes = 4096;
constexpr size_t kMaxContentIdBytes = 255;

struct DispatchedTask {
  MigoTaskFn function;
  void *context;
};

void run_dispatched_task(napi_env env, napi_value js_callback, void *context,
                         void *data) {
  (void)env;
  (void)js_callback;
  (void)context;
  std::unique_ptr<DispatchedTask> task(static_cast<DispatchedTask *>(data));
  /* Even when ArkTS is shutting down and env is null, invoke the Migo task
   * exactly once. Session teardown converts it into a cancellation wrapper
   * that only releases engine-owned storage and cannot touch user_data. */
  if (task != nullptr && task->function != nullptr) {
    task->function(task->context);
  }
}

void finalize_dispatcher(napi_env env, void *finalize_data,
                         void *finalize_hint) {
  (void)env;
  (void)finalize_hint;
  Host *host = static_cast<Host *>(finalize_data);
  if (host != nullptr) {
    std::lock_guard<std::mutex> lock(host->state_mutex);
    host->dispatcher = nullptr;
  }
}

bool ensure_dispatcher(napi_env env) {
  std::lock_guard<std::mutex> init_lock(g_dispatcher_init_mutex);
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    if (g_host.dispatcher != nullptr) {
      return true;
    }
  }

  napi_value resource_name = nullptr;
  if (napi_create_string_utf8(env, "Migo host dispatcher", NAPI_AUTO_LENGTH,
                              &resource_name) != napi_ok) {
    return false;
  }
  napi_threadsafe_function dispatcher = nullptr;
  napi_status status = napi_create_threadsafe_function(
      env, nullptr, nullptr, resource_name,
      256, /* bounded: a stalled UI loop cannot retain unbounded callbacks */
      1, &g_host, finalize_dispatcher, &g_host, run_dispatched_task,
      &dispatcher);
  if (status != napi_ok || dispatcher == nullptr) {
    return false;
  }
  /* The dispatcher must not keep the ArkTS event loop alive after the page
   * and its native module would otherwise be collectible. */
  if (napi_unref_threadsafe_function(env, dispatcher) != napi_ok) {
    napi_release_threadsafe_function(dispatcher, napi_tsfn_abort);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    g_host.dispatcher = dispatcher;
  }
  return true;
}

bool try_finalize_surface_release(Host *host);
void attach_surface(OH_NativeXComponent *component, void *window);
void attach_pending_surface_if_ready();
MigoResult finish_stop_if_ready();

void on_surface_released(void *user_data, MigoSession *session,
                         uint64_t generation) {
  (void)session;
  Host *host = static_cast<Host *>(user_data);
  if (host == nullptr) {
    return;
  }

  bool accepted = false;
  {
    std::lock_guard<std::mutex> lock(host->state_mutex);
    /* The generation check also rejects a delayed edge from an older
     * attachment after a new detach has started. */
    const uint64_t expected = host->release_generation != 0
                                  ? host->release_generation
                                  : host->detaching_generation;
    if (!host->detach_in_progress || expected == 0 || generation != expected) {
      return;
    }
    host->release_callback_seen = true;
    host->release_callback_generation = generation;
    accepted = true;
  }

  /* Query is level-triggered and non-blocking. It is deliberately outside the
   * state mutex so an engine/render callback can never stall the UI callback.
   */
  bool finalized = false;
  if (accepted) {
    std::lock_guard<std::mutex> api_lock(host->api_mutex);
    finalized = try_finalize_surface_release(host);
  }
  if (finalized) {
    if (finish_stop_if_ready() == MIGO_ERROR_INVALID_STATE) {
      attach_pending_surface_if_ready();
    }
  }
}

bool try_finalize_surface_release(Host *host) {
  MigoSurfaceRelease *release = nullptr;
  uint64_t expected_generation = 0;
  {
    std::lock_guard<std::mutex> lock(host->state_mutex);
    if (host->release == nullptr || host->release_completion_claimed) {
      return false;
    }
    host->release_completion_claimed = true;
    release = host->release;
    expected_generation = host->release_generation;
  }

  MigoSurfaceReleaseStatus status;
  memset(&status, 0, sizeof status);
  status.struct_size = (uint32_t)sizeof status;
  status.abi_version = MIGO_ABI_VERSION_CURRENT;
  MigoResult rc = migo_surface_release_query(release, &status);
  if (rc != MIGO_OK) {
    LOGE("migo_surface_release_query failed: %{public}d", (int)rc);
    std::lock_guard<std::mutex> lock(host->state_mutex);
    if (host->release == release) {
      host->release_completion_claimed = false;
    }
    return false;
  }
  if (status.generation != expected_generation ||
      status.state != MIGO_SURFACE_RELEASE_RELEASED) {
    /* Keep ownership: a pending or mismatched observer is never destroyed.
     * The next host event can retry the authoritative query. */
    std::lock_guard<std::mutex> lock(host->state_mutex);
    if (host->release == release) {
      host->release_completion_claimed = false;
    }
    return false;
  }

  rc = migo_surface_release_destroy(release);
  if (rc != MIGO_OK) {
    LOGE("migo_surface_release_destroy failed: %{public}d", (int)rc);
    std::lock_guard<std::mutex> lock(host->state_mutex);
    if (host->release == release) {
      host->release_completion_claimed = false;
    }
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(host->state_mutex);
    if (host->release == release) {
      host->release = nullptr;
      host->release_generation = 0;
      host->release_callback_generation = 0;
      host->release_callback_seen = false;
      host->release_completion_claimed = false;
      host->detaching_generation = 0;
      host->detach_in_progress = false;
    }
  }
  LOGI("surface released");
  return true;
}

/*
 * Every user callback must be delivered through a host-owned dispatcher: the
 * engine produces these on its own worker threads. A bounded N-API thread-safe
 * function moves them onto the ArkTS event loop without blocking an engine
 * worker and without retaining an unbounded callback backlog.
 */
MigoResult dispatch_to_arkts(void *dispatcher_context, MigoTaskFn task,
                             void *task_context) {
  Host *host = static_cast<Host *>(dispatcher_context);
  if (task == nullptr) {
    return MIGO_ERROR_INVALID_ARGUMENT;
  }
  napi_threadsafe_function dispatcher = nullptr;
  if (host != nullptr) {
    std::lock_guard<std::mutex> lock(host->state_mutex);
    dispatcher = host->dispatcher;
  }
  if (dispatcher == nullptr) {
    return MIGO_ERROR_INVALID_STATE;
  }
  std::unique_ptr<DispatchedTask> pending(
      new (std::nothrow) DispatchedTask{task, task_context});
  if (pending == nullptr) {
    return MIGO_ERROR_INTERNAL;
  }
  napi_status status = napi_call_threadsafe_function(dispatcher, pending.get(),
                                                     napi_tsfn_nonblocking);
  if (status == napi_ok) {
    (void)pending.release();
    return MIGO_OK;
  }
  return status == napi_queue_full ? MIGO_ERROR_WOULD_BLOCK
                                   : MIGO_ERROR_INTERNAL;
}

void on_ready(void *user_data, MigoSession *session) {
  (void)user_data;
  (void)session;
  LOGI("content is ready");
}

void on_error(void *user_data, MigoSession *session, const MigoError *error) {
  (void)user_data;
  (void)session;
  /* Engine messages may contain a sandbox path. Keep production diagnostics
   * stable and non-sensitive; the numeric ABI code is sufficient for routing.
   */
  if (error != nullptr) {
    LOGE("engine error %{public}d", (int)error->code);
  } else {
    LOGE("engine error with no message");
  }
}

void on_exit_requested(void *user_data, MigoSession *session) {
  (void)user_data;
  (void)session;
  LOGI("content requested exit");
}

void attach_surface(OH_NativeXComponent *component, void *window) {
  std::lock_guard<std::mutex> api_lock(g_host.api_mutex);
  /* A platform may recreate the component before the release edge's task is
   * scheduled. Retry only the non-blocking authoritative query here; never
   * sleep waiting for the driver. */
  try_finalize_surface_release(&g_host);
  /* Every early return says why. A silent one here is how a surface that
   * never attaches presents as an ordinary black screen -- the exact failure
   * mode this host exists to rule out. */
  if (window == nullptr) {
    LOGE("attach skipped: null window");
    return;
  }
  MigoSession *session = nullptr;
  uint64_t generation = 0;
  float scale_factor = 1.0f;
  bool content_loaded = false;
  std::string content_id;
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    if (g_host.start_state == StartState::Stopping) {
      LOGI("attach skipped: host teardown is in progress");
      return;
    }
    if (g_host.attachment != nullptr) {
      LOGI("attach skipped: already attached");
      return;
    }
    if (g_host.release != nullptr || g_host.detach_in_progress) {
      LOGI("surface arrived while the previous release is pending; deferring "
           "attach");
      g_host.pending_component = component;
      g_host.pending_window = window;
      return;
    }
    if (g_host.session == nullptr) {
      LOGI("surface arrived before the engine; deferring attach");
      g_host.pending_component = component;
      g_host.pending_window = window;
      return;
    }
    session = g_host.session;
    generation = ++g_host.generation;
    scale_factor = g_host.scale_factor;
    content_loaded = g_host.content_loaded;
    content_id = g_host.content_id;
  }

  uint64_t width = 0;
  uint64_t height = 0;
  if (OH_NativeXComponent_GetXComponentSize(component, window, &width,
                                            &height) != 0) {
    LOGE("OH_NativeXComponent_GetXComponentSize failed");
    return;
  }
  if (width == 0 || height == 0 || width > UINT32_MAX || height > UINT32_MAX) {
    LOGE("surface dimensions are outside the C ABI range");
    return;
  }
  LOGI("surface created %{public}llu x %{public}llu", (unsigned long long)width,
       (unsigned long long)height);

  MigoOpenHarmonyNativeWindowDescriptor native;
  memset(&native, 0, sizeof native);
  native.struct_size = (uint32_t)sizeof native;
  native.abi_version = MIGO_ABI_VERSION_CURRENT;
  native.platform_kind = MIGO_PLATFORM_OPENHARMONY_NATIVE_WINDOW;
  native.flags = 0;
  /* The engine takes its own reference; this one stays ours. */
  native.native_window = window;

  MigoSurfaceDescriptor surface;
  memset(&surface, 0, sizeof surface);
  surface.struct_size = (uint32_t)sizeof surface;
  surface.abi_version = MIGO_ABI_VERSION_CURRENT;
  /* Generations are monotonic per Session and never reused, so a stale
   * attachment can be told apart from the live one. */
  surface.generation = generation;
  surface.platform_kind = MIGO_PLATFORM_OPENHARMONY_NATIVE_WINDOW;
  surface.flags = 0;
  surface.width_pixels = (uint32_t)width;
  surface.height_pixels = (uint32_t)height;
  /* Physical pixels per CSS pixel. A wrong value here still renders, but puts
   * every touch in the wrong place -- the failure is silent and looks like an
   * input bug rather than a configuration one. */
  surface.scale_factor = scale_factor;
  surface.color_space = MIGO_COLOR_SPACE_SRGB;
  surface.alpha_mode = MIGO_ALPHA_MODE_OPAQUE;
  surface.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT;
  surface.capability_flags = 0;
  surface.platform_descriptor_size = (uint32_t)sizeof native;
  surface.platform_descriptor = &native;

  MigoSurfaceAttachment *attachment = nullptr;
  MigoResult rc = migo_session_attach_surface(session, &surface, &attachment);
  if (rc != MIGO_OK) {
    LOGE("migo_session_attach_surface failed: %{public}d", (int)rc);
    return;
  }
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    g_host.attachment = attachment;
    g_host.pending_component = nullptr;
    g_host.pending_window = nullptr;
  }
  LOGI("surface attached, generation %{public}llu",
       (unsigned long long)surface.generation);

  if (!content_loaded && !content_id.empty()) {
    MigoContentDescriptor content;
    memset(&content, 0, sizeof content);
    content.struct_size = (uint32_t)sizeof content;
    content.abi_version = MIGO_ABI_VERSION_CURRENT;
    content.flags = 0;
    content.content_id_utf8 = content_id.c_str();
    /* Not optional: the engine rejects a null entry with
     * MIGO_ERROR_INVALID_ARGUMENT. Both the Android and the Linux host name
     * game.js here, and mini-game content always has one. */
    content.entry_utf8 = "game.js";

    rc = migo_session_load_content(session, &content);
    if (rc != MIGO_OK) {
      LOGE("migo_session_load_content failed: %{public}d", (int)rc);
    } else {
      std::lock_guard<std::mutex> lock(g_host.state_mutex);
      g_host.content_loaded = true;
      LOGI("loading content %{public}s", content_id.c_str());
    }
  }
}

void attach_pending_surface_if_ready() {
  OH_NativeXComponent *component = nullptr;
  void *window = nullptr;
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    if (g_host.session == nullptr || g_host.attachment != nullptr ||
        g_host.release != nullptr || g_host.detach_in_progress) {
      return;
    }
    component = g_host.pending_component;
    window = g_host.pending_window;
  }
  if (component != nullptr && window != nullptr) {
    attach_surface(component, window);
  }
}

MigoResult detach_surface() {
  std::lock_guard<std::mutex> api_lock(g_host.api_mutex);
  MigoSurfaceAttachment *attachment = nullptr;
  uint64_t generation = 0;
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    if (g_host.attachment == nullptr) {
      return (g_host.detach_in_progress || g_host.release != nullptr)
                 ? MIGO_ERROR_WOULD_BLOCK
                 : MIGO_OK;
    }
    if (g_host.detach_in_progress || g_host.release != nullptr) {
      LOGI("surface detach already in progress");
      return MIGO_ERROR_WOULD_BLOCK;
    }
    attachment = g_host.attachment;
    generation = g_host.generation;
    g_host.detaching_generation = generation;
    g_host.release_callback_generation = 0;
    g_host.release_callback_seen = false;
    g_host.release_completion_claimed = false;
    g_host.detach_in_progress = true;
  }

  MigoSurfaceRelease *release = nullptr;
  MigoResult rc = migo_surface_begin_detach(attachment, &release);
  if (rc != MIGO_OK) {
    LOGE("migo_surface_begin_detach failed: %{public}d", (int)rc);
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    g_host.detaching_generation = 0;
    g_host.release_callback_generation = 0;
    g_host.release_callback_seen = false;
    g_host.release_completion_claimed = false;
    g_host.detach_in_progress = false;
    return rc;
  }

  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    /* MIGO_OK consumes the attachment. Publication is intentionally after
     * begin_detach: the release callback may have run synchronously while
     * begin_detach was dropping its last resource lease. */
    g_host.attachment = nullptr;
    g_host.release = release;
    g_host.release_generation = generation;
    if (release == nullptr) {
      LOGE("migo_surface_begin_detach returned no release observer");
      g_host.release_generation = 0;
      g_host.release_callback_generation = 0;
      g_host.release_callback_seen = false;
      g_host.release_completion_claimed = false;
      g_host.detaching_generation = 0;
      g_host.detach_in_progress = false;
    }
  }
  const bool finalized =
      release != nullptr && try_finalize_surface_release(&g_host);
  if (release == nullptr) {
    return MIGO_ERROR_INTERNAL;
  }
  return finalized ? MIGO_OK : MIGO_ERROR_WOULD_BLOCK;
}

MigoResult finish_stop_if_ready() {
  std::lock_guard<std::mutex> api_lock(g_host.api_mutex);
  MigoSession *session = nullptr;
  MigoEngine *engine = nullptr;
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    if (g_host.start_state != StartState::Stopping) {
      return MIGO_ERROR_INVALID_STATE;
    }
    if (g_host.teardown_claimed || g_host.attachment != nullptr ||
        g_host.release != nullptr || g_host.detach_in_progress) {
      return MIGO_ERROR_WOULD_BLOCK;
    }
    session = g_host.session;
    engine = g_host.engine;
    if (session == nullptr && engine == nullptr) {
      g_host.start_state = StartState::Stopped;
      return MIGO_OK;
    }
    g_host.teardown_claimed = true;
  }

  if (session != nullptr) {
    /* Stop retracts input and hides content before closing the callback
     * gate. These level calls are best-effort cleanup: destruction is the
     * authority and must still be attempted if a worker has already gone. */
    MigoResult rc = migo_session_set_focus(session, 0);
    if (rc != MIGO_OK) {
      LOGE("stop focus transition failed: %{public}d", (int)rc);
    }
    rc = migo_session_set_visibility(session, 0);
    if (rc != MIGO_OK) {
      LOGE("stop visibility transition failed: %{public}d", (int)rc);
    }
    rc = migo_session_set_lifecycle(session, MIGO_LIFECYCLE_PAUSED);
    if (rc != MIGO_OK) {
      LOGE("stop lifecycle transition failed: %{public}d", (int)rc);
    }
    rc = migo_session_destroy(session);
    if (rc != MIGO_OK) {
      LOGE("migo_session_destroy failed: %{public}d", (int)rc);
      std::lock_guard<std::mutex> lock(g_host.state_mutex);
      g_host.teardown_claimed = false;
      return rc;
    }
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    if (g_host.session == session) {
      g_host.session = nullptr;
    }
  }

  if (engine != nullptr) {
    MigoResult rc = migo_engine_destroy(engine);
    if (rc != MIGO_OK) {
      LOGE("migo_engine_destroy failed: %{public}d", (int)rc);
      std::lock_guard<std::mutex> lock(g_host.state_mutex);
      g_host.teardown_claimed = false;
      return rc;
    }
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    if (g_host.engine == engine) {
      g_host.engine = nullptr;
    }
  }

  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    g_host.teardown_claimed = false;
    g_host.content_loaded = false;
    g_host.foreground_state_valid = false;
    g_host.pending_component = nullptr;
    g_host.pending_window = nullptr;
    g_host.files_dir.clear();
    g_host.cache_dir.clear();
    g_host.content_id.clear();
    g_host.generation = 0;
    g_host.start_state = StartState::Stopped;
  }
  LOGI("engine and session stopped");
  return MIGO_OK;
}

void OnSurfaceCreatedCB(OH_NativeXComponent *component, void *window) {
  attach_surface(component, window);
}

void OnSurfaceChangedCB(OH_NativeXComponent *component, void *window) {
  std::unique_lock<std::mutex> api_lock(g_host.api_mutex);
  (void)try_finalize_surface_release(&g_host);

  MigoSurfaceAttachment *attachment = nullptr;
  uint64_t generation = 0;
  float scale_factor = 1.0f;
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    attachment = g_host.attachment;
    generation = g_host.generation;
    scale_factor = g_host.scale_factor;
  }
  if (attachment == nullptr) {
    api_lock.unlock();
    attach_surface(component, window);
    return;
  }

  uint64_t width = 0;
  uint64_t height = 0;
  if (OH_NativeXComponent_GetXComponentSize(component, window, &width,
                                            &height) != 0 ||
      width == 0 || height == 0 || width > UINT32_MAX || height > UINT32_MAX) {
    LOGE("surface change has invalid dimensions");
    return;
  }

  MigoSurfaceMetrics metrics;
  memset(&metrics, 0, sizeof metrics);
  metrics.struct_size = (uint32_t)sizeof metrics;
  metrics.abi_version = MIGO_ABI_VERSION_CURRENT;
  metrics.generation = generation;
  metrics.width_pixels = (uint32_t)width;
  metrics.height_pixels = (uint32_t)height;
  metrics.scale_factor = scale_factor;
  metrics.color_space = MIGO_COLOR_SPACE_SRGB;
  metrics.alpha_mode = MIGO_ALPHA_MODE_OPAQUE;
  metrics.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT;
  metrics.flags = 0;
  MigoResult rc = migo_surface_update(attachment, &metrics);
  if (rc != MIGO_OK && rc != MIGO_ERROR_STALE_SURFACE) {
    LOGE("migo_surface_update failed: %{public}d", (int)rc);
  }
}

void OnSurfaceDestroyedCB(OH_NativeXComponent *component, void *window) {
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    /* A Surface can be created and destroyed before ArkTS starts the Engine.
     * Clear only the matching deferred pair: a later Surface may already be
     * waiting while an older generation reports its destruction. */
    if (g_host.pending_component == component &&
        g_host.pending_window == window) {
      g_host.pending_component = nullptr;
      g_host.pending_window = nullptr;
    }
  }
  (void)detach_surface();
}

/*
 * Touch, translated rather than forwarded.
 *
 * Two things are easy to get wrong here and neither fails loudly:
 *   - Coordinates cross the ABI in CSS pixels, while OpenHarmony reports
 *     physical ones. Skipping the division renders correctly and puts every
 *     touch in the wrong place.
 *   - The event type belongs to the whole event; per-point types exist but a
 * mini-game content model expects one phase per delivery, which is what the
 * engine's MIGO_TOUCH_* values encode.
 */
void DispatchTouchEventCB(OH_NativeXComponent *component, void *window) {
  std::lock_guard<std::mutex> api_lock(g_host.api_mutex);
  MigoSession *session = nullptr;
  float scale_factor = 1.0f;
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    if (g_host.start_state != StartState::Started) {
      return;
    }
    session = g_host.session;
    scale_factor = g_host.scale_factor;
  }
  if (session == nullptr) {
    return;
  }
  OH_NativeXComponent_TouchEvent event;
  memset(&event, 0, sizeof event);
  if (OH_NativeXComponent_GetTouchEvent(component, window, &event) != 0) {
    LOGE("OH_NativeXComponent_GetTouchEvent failed");
    return;
  }

  MigoTouchType type;
  switch (event.type) {
  case OH_NATIVEXCOMPONENT_DOWN:
    type = MIGO_TOUCH_START;
    break;
  case OH_NATIVEXCOMPONENT_MOVE:
    type = MIGO_TOUCH_MOVE;
    break;
  case OH_NATIVEXCOMPONENT_UP:
    type = MIGO_TOUCH_END;
    break;
  case OH_NATIVEXCOMPONENT_CANCEL:
    type = MIGO_TOUCH_CANCEL;
    break;
  default:
    return; /* UNKNOWN carries no phase the engine can act on. */
  }

  uint32_t count = event.numPoints;
  if (count > MIGO_TOUCH_MAX_POINTS)
    count = MIGO_TOUCH_MAX_POINTS;

  MigoTouchPoint points[MIGO_TOUCH_MAX_POINTS];
  memset(points, 0, sizeof points);
  const float inv_scale = (scale_factor > 0.0f) ? (1.0f / scale_factor) : 1.0f;

  /*
   * Two independent flags, and getting either wrong is silent.
   *
   * MIGO_TOUCH_FLAG_CHANGED selects `changedTouches`. For a move every point
   * changed at once; for the pointer-specific phases exactly one did, and
   * OpenHarmony names it in the event's own id field.
   *
   * MIGO_TOUCH_FLAG_REMOVED is what takes a point *out* of `touches`, and it
   * is a separate decision -- the engine's JS keeps every point that is not
   * flagged removed, regardless of the event phase. Sending a touchend whose
   * point carries only CHANGED delivers an end event in which the lifted
   * finger is still listed as on the surface, so content waiting for
   * `touches.length === 0` never sees it. That is the bug the probe caught:
   * it turned green on touchstart and stayed green after the finger lifted.
   *
   * Which point left is decided from the event's id, and the per-point `type`
   * field is deliberately not used for it. That field looks like the direct
   * answer and is not: on an UP event the emulator reports the lifted point's
   * own type as MOVE, not UP (logged below, API 20 / Mate 70 Pro emulator).
   * Testing it would remove nothing and reproduce exactly the bug this comment
   * describes. `isPressed` does go false on the same event and agrees with the
   * rule used here; it is left as a cross-check rather than the source of
   * truth because one signal deciding it keeps start and end symmetric.
   *
   * Multi-finger behaviour is unverified on a device: hdc cannot synthesise a
   * second pointer. The per-point lines below are what a real multi-touch
   * session would be read against.
   */
  const bool all_changed = (type == MIGO_TOUCH_MOVE);
  const bool phase_removes =
      (type == MIGO_TOUCH_END || type == MIGO_TOUCH_CANCEL);
  LOGI("touch type=%{public}u numPoints=%{public}u subject.id=%{public}d",
       (unsigned)type, event.numPoints, event.id);
  for (uint32_t i = 0; i < count; ++i) {
    const OH_NativeXComponent_TouchPoint &tp = event.touchPoints[i];
    const bool is_subject = (tp.id == event.id);
    points[i].id = (uint32_t)tp.id;
    points[i].x = tp.x * inv_scale;
    points[i].y = tp.y * inv_scale;
    points[i].pressure = tp.force;
    points[i].flags = 0;
    if (all_changed || is_subject)
      points[i].flags |= MIGO_TOUCH_FLAG_CHANGED;
    if (phase_removes && is_subject)
      points[i].flags |= MIGO_TOUCH_FLAG_REMOVED;
    LOGI("  point[%{public}u] id=%{public}d type=%{public}d pressed=%{public}d "
         "flags=0x%{public}x",
         i, tp.id, (int)tp.type, (int)tp.isPressed, (unsigned)points[i].flags);
  }

  /*
   * An event with no points still has to be delivered. The event itself
   * carries the pointer's id and position, so it is described here from its
   * own fields rather than dropped -- dropping an end leaves content believing
   * a finger is still down, and no later event corrects that.
   */
  if (count == 0) {
    LOGI("  event carries no points; describing it from the event fields");
    count = 1;
    points[0].id = (uint32_t)event.id;
    points[0].x = event.x * inv_scale;
    points[0].y = event.y * inv_scale;
    points[0].pressure = event.force;
    points[0].flags = MIGO_TOUCH_FLAG_CHANGED |
                      (phase_removes ? MIGO_TOUCH_FLAG_REMOVED : 0u);
  }

  MigoTouchEvent out;
  memset(&out, 0, sizeof out);
  out.struct_size = (uint32_t)sizeof out;
  out.abi_version = MIGO_ABI_VERSION_CURRENT;
  out.type = type;
  out.point_count = count;
  out.timestamp_ms = event.timeStamp / 1000000; /* ns -> ms */
  out.points = points;

  MigoResult rc = migo_session_send_touch(session, &out);
  if (rc != MIGO_OK) {
    /* WOULD_BLOCK is transient and the host decides whether to retry;
     * dropping an END silently would leave content believing a finger is
     * still down, with no later event to correct it. */
    LOGE("migo_session_send_touch(type=%{public}u) failed: %{public}d",
         (unsigned)type, (int)rc);
  }
}

OH_NativeXComponent_Callback g_callbacks = {
    OnSurfaceCreatedCB,
    OnSurfaceChangedCB,
    OnSurfaceDestroyedCB,
    DispatchTouchEventCB,
};

std::string read_string_arg(napi_env env, napi_value value, size_t max_length) {
  size_t len = 0;
  if (napi_get_value_string_utf8(env, value, nullptr, 0, &len) != napi_ok ||
      len == 0 || len > max_length) {
    return {};
  }
  std::string out;
  try {
    out.resize(len + 1, '\0');
  } catch (...) {
    return {};
  }
  size_t written = 0;
  if (napi_get_value_string_utf8(env, value, &out[0], len + 1, &written) !=
      napi_ok) {
    return {};
  }
  out.resize(written);
  if (out.find('\0') != std::string::npos) {
    return {};
  }
  return out;
}

napi_value result_value(napi_env env, MigoResult result) {
  napi_value out = nullptr;
  napi_create_int32(env, (int32_t)result, &out);
  return out;
}

MigoResult apply_foreground_state(MigoSession *session, bool foreground) {
  MigoResult first_error = MIGO_OK;
  const auto record = [&first_error](MigoResult result) {
    if (first_error == MIGO_OK && result != MIGO_OK) {
      first_error = result;
    }
  };
  if (foreground) {
    record(migo_session_set_lifecycle(session, MIGO_LIFECYCLE_RUNNING));
    record(migo_session_set_visibility(session, 1));
    record(migo_session_set_focus(session, 1));
  } else {
    /* Retract input before hiding/pausing so content never retains a key or
     * pointer merely because the Ability went to the background. */
    record(migo_session_set_focus(session, 0));
    record(migo_session_set_visibility(session, 0));
    record(migo_session_set_lifecycle(session, MIGO_LIFECYCLE_PAUSED));
  }
  return first_error;
}

void rollback_start(MigoEngine *engine, MigoSession *session) {
  if (session != nullptr) {
    MigoResult rc = migo_session_destroy(session);
    if (rc != MIGO_OK) {
      LOGE("rollback migo_session_destroy failed: %{public}d", (int)rc);
    }
  }
  if (engine != nullptr) {
    MigoResult rc = migo_engine_destroy(engine);
    if (rc != MIGO_OK) {
      LOGE("rollback migo_engine_destroy failed: %{public}d", (int)rc);
    }
  }
  std::lock_guard<std::mutex> lock(g_host.state_mutex);
  g_host.engine = nullptr;
  g_host.session = nullptr;
  g_host.teardown_claimed = false;
  g_host.content_loaded = false;
  g_host.foreground_state_valid = false;
  g_host.files_dir.clear();
  g_host.cache_dir.clear();
  g_host.content_id.clear();
  g_host.start_state = StartState::Stopped;
}

/* start(filesDir: string, cacheDir: string, contentId: string,
 *       scaleFactor: number): number */
napi_value Start(napi_env env, napi_callback_info info) {
  size_t argc = 4;
  napi_value args[4] = {nullptr, nullptr, nullptr, nullptr};
  if (napi_get_cb_info(env, info, &argc, args, nullptr, nullptr) != napi_ok ||
      argc != 4) {
    return result_value(env, MIGO_ERROR_INVALID_ARGUMENT);
  }

  napi_value out = nullptr;
  const std::string files_dir =
      argc >= 1 ? read_string_arg(env, args[0], kMaxPathBytes) : std::string();
  const std::string cache_dir =
      argc >= 2 ? read_string_arg(env, args[1], kMaxPathBytes) : std::string();
  const std::string content_id =
      argc >= 3 ? read_string_arg(env, args[2], kMaxContentIdBytes)
                : std::string();
  if (files_dir.empty() || cache_dir.empty() || content_id.empty()) {
    return result_value(env, MIGO_ERROR_INVALID_ARGUMENT);
  }
  double scale_factor = 0.0;
  if (napi_get_value_double(env, args[3], &scale_factor) != napi_ok ||
      !std::isfinite(scale_factor) || scale_factor <= 0.0 ||
      scale_factor > 16.0) {
    return result_value(env, MIGO_ERROR_INVALID_ARGUMENT);
  }

  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    if (g_host.dispatcher == nullptr) {
      napi_create_int32(env, (int32_t)MIGO_ERROR_INTERNAL, &out);
      return out;
    }
    if (g_host.start_state != StartState::Stopped) {
      napi_create_int32(env, (int32_t)MIGO_ERROR_INVALID_STATE, &out);
      return out;
    }
    g_host.files_dir = files_dir;
    g_host.cache_dir = cache_dir;
    g_host.content_id = content_id;
    g_host.scale_factor = (float)scale_factor;
    g_host.content_loaded = false;
    g_host.foreground_state_valid = false;
    g_host.teardown_claimed = false;
    g_host.start_state = StartState::Starting;
  }

  MigoEngineConfig config;
  memset(&config, 0, sizeof config);
  config.struct_size = (uint32_t)sizeof config;
  config.abi_version = MIGO_ABI_VERSION_CURRENT;
  /* Production is always signed-content-only. The opt-in is deliberately a
   * compile definition supplied only by the Debug build profile. */
  config.flags = 0;
#if defined(MIGO_OHOS_ALLOW_UNSIGNED_CONTENT) &&                               \
    MIGO_OHOS_ALLOW_UNSIGNED_CONTENT
  config.flags = MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT;
#endif
  config.files_dir_utf8 = files_dir.c_str();
  config.cache_dir_utf8 = cache_dir.c_str();
  config.code_cache_dir_utf8 = cache_dir.c_str();

  MigoEngine *engine = nullptr;
  MigoResult rc = migo_engine_create(&config, &engine);
  if (rc != MIGO_OK) {
    LOGE("migo_engine_create failed: %{public}d", (int)rc);
    rollback_start(nullptr, nullptr);
    napi_create_int32(env, (int32_t)rc, &out);
    return out;
  }

  MigoSessionConfig session_config;
  memset(&session_config, 0, sizeof session_config);
  session_config.struct_size = (uint32_t)sizeof session_config;
  session_config.abi_version = MIGO_ABI_VERSION_CURRENT;

  MigoSession *session = nullptr;
  rc = migo_session_create(engine, &session_config, &session);
  if (rc != MIGO_OK) {
    LOGE("migo_session_create failed: %{public}d", (int)rc);
    rollback_start(engine, nullptr);
    napi_create_int32(env, (int32_t)rc, &out);
    return out;
  }

  /* Callbacks install once, before the first attach: replacing them later
   * would race queued tasks against the function pointers they captured. */
  MigoHostCallbacks callbacks;
  memset(&callbacks, 0, sizeof callbacks);
  callbacks.struct_size = (uint32_t)sizeof callbacks;
  callbacks.abi_version = MIGO_ABI_VERSION_CURRENT;
  callbacks.user_data = &g_host;
  callbacks.dispatcher_data = &g_host;
  callbacks.dispatch = dispatch_to_arkts;
  callbacks.on_ready = on_ready;
  callbacks.on_error = on_error;
  callbacks.on_exit_requested = on_exit_requested;
  callbacks.on_surface_released = on_surface_released;
  rc = migo_session_set_host_callbacks(session, &callbacks);
  if (rc != MIGO_OK) {
    LOGE("migo_session_set_host_callbacks failed: %{public}d", (int)rc);
    rollback_start(engine, session);
    napi_create_int32(env, (int32_t)rc, &out);
    return out;
  }

  bool foreground = true;
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    foreground = g_host.foreground;
  }
  rc = apply_foreground_state(session, foreground);
  if (rc != MIGO_OK) {
    LOGE("initial lifecycle transition failed: %{public}d", (int)rc);
    rollback_start(engine, session);
    return result_value(env, rc);
  }

  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    g_host.engine = engine;
    g_host.session = session;
    g_host.applied_foreground = foreground;
    g_host.foreground_state_valid = true;
    g_host.start_state = StartState::Started;
  }
  LOGI("engine and session created");

  /* If the surface won the race, attach it now. */
  OH_NativeXComponent *pending_component = nullptr;
  void *pending_window = nullptr;
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    pending_component = g_host.pending_component;
    pending_window = g_host.pending_window;
  }
  if (pending_window != nullptr) {
    LOGI("attaching the surface that arrived first");
    attach_surface(pending_component, pending_window);
  }

  napi_create_int32(env, (int32_t)MIGO_OK, &out);
  return out;
}

/* setForeground(foreground: boolean): number */
napi_value SetForeground(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value arg = nullptr;
  if (napi_get_cb_info(env, info, &argc, &arg, nullptr, nullptr) != napi_ok ||
      argc != 1) {
    return result_value(env, MIGO_ERROR_INVALID_ARGUMENT);
  }
  bool foreground = false;
  if (napi_get_value_bool(env, arg, &foreground) != napi_ok) {
    return result_value(env, MIGO_ERROR_INVALID_ARGUMENT);
  }

  std::lock_guard<std::mutex> api_lock(g_host.api_mutex);
  MigoSession *session = nullptr;
  bool needs_apply = false;
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    g_host.foreground = foreground;
    if (g_host.start_state == StartState::Started) {
      session = g_host.session;
      needs_apply = !g_host.foreground_state_valid ||
                    g_host.applied_foreground != foreground;
    }
  }
  if (session == nullptr || !needs_apply) {
    return result_value(env, MIGO_OK);
  }
  const MigoResult result = apply_foreground_state(session, foreground);
  if (result == MIGO_OK) {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    if (g_host.session == session && g_host.foreground == foreground) {
      g_host.applied_foreground = foreground;
      g_host.foreground_state_valid = true;
    }
  }
  return result_value(env, result);
}

/* stop(): number
 *
 * MIGO_ERROR_WOULD_BLOCK means detach was accepted and the release observer
 * has not reached RELEASED yet. The release callback completes teardown; a
 * later call is also a safe, level-triggered poll if that notification was
 * rejected by the bounded dispatcher. */
napi_value Stop(napi_env env, napi_callback_info info) {
  size_t argc = 0;
  if (napi_get_cb_info(env, info, &argc, nullptr, nullptr, nullptr) !=
          napi_ok ||
      argc != 0) {
    return result_value(env, MIGO_ERROR_INVALID_ARGUMENT);
  }
  {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    if (g_host.start_state == StartState::Stopped) {
      return result_value(env, MIGO_OK);
    }
    if (g_host.start_state == StartState::Starting) {
      return result_value(env, MIGO_ERROR_INVALID_STATE);
    }
    if (g_host.start_state == StartState::Started) {
      g_host.start_state = StartState::Stopping;
      g_host.pending_component = nullptr;
      g_host.pending_window = nullptr;
    }
  }

  MigoResult detach_result = detach_surface();
  if (detach_result != MIGO_OK && detach_result != MIGO_ERROR_WOULD_BLOCK) {
    return result_value(env, detach_result);
  }
  if (detach_result == MIGO_ERROR_WOULD_BLOCK) {
    std::lock_guard<std::mutex> api_lock(g_host.api_mutex);
    (void)try_finalize_surface_release(&g_host);
  }
  MigoResult result = finish_stop_if_ready();
  if (result == MIGO_ERROR_INVALID_STATE) {
    std::lock_guard<std::mutex> lock(g_host.state_mutex);
    if (g_host.start_state == StartState::Stopped) {
      result = MIGO_OK;
    }
  }
  return result_value(env, result);
}

napi_value Init(napi_env env, napi_value exports) {
  if (!ensure_dispatcher(env)) {
    LOGE("failed to create the bounded ArkTS dispatcher");
  }
  napi_property_descriptor desc[] = {
      {"start", nullptr, Start, nullptr, nullptr, nullptr, napi_default,
       nullptr},
      {"stop", nullptr, Stop, nullptr, nullptr, nullptr, napi_default, nullptr},
      {"setForeground", nullptr, SetForeground, nullptr, nullptr, nullptr,
       napi_default, nullptr},
  };
  napi_define_properties(env, exports, sizeof(desc) / sizeof(desc[0]), desc);

  /* Bind to the XComponent declared in the ArkTS page. Without this the
   * surface callbacks never fire and the engine is handed nothing to draw
   * on -- which presents as a silent black screen, not as an error.
   *
   * ArkUI calls this function more than once: once when the module itself is
   * registered, before any XComponent exists, and again once a component has
   * been bound to it. The first pass therefore finds nothing to unwrap, and
   * that is the normal path, not a fault. It used to be logged at error level,
   * so every healthy launch printed a failure -- on a platform where this log
   * is the only diagnostic channel a host has, a permanent false error is
   * worse than no message, because it teaches the reader to skip errors.
   *
   * The signal that matters is positive: "surface callbacks registered" must
   * appear. If it never does, nothing below will run and the screen stays
   * black with no error anywhere. */
  napi_value exportInstance = nullptr;
  if (napi_get_named_property(env, exports, OH_NATIVE_XCOMPONENT_OBJ,
                              &exportInstance) == napi_ok) {
    OH_NativeXComponent *component = nullptr;
    if (napi_unwrap(env, exportInstance,
                    reinterpret_cast<void **>(&component)) == napi_ok &&
        component != nullptr) {
      char id[OH_XCOMPONENT_ID_LEN_MAX + 1] = {};
      uint64_t id_len = OH_XCOMPONENT_ID_LEN_MAX + 1;
      if (OH_NativeXComponent_GetXComponentId(component, id, &id_len) == 0) {
        LOGI("bound XComponent id=%{public}s", id);
      }
      int32_t reg =
          OH_NativeXComponent_RegisterCallback(component, &g_callbacks);
      if (reg != 0) {
        LOGE("OH_NativeXComponent_RegisterCallback failed: %{public}d", reg);
      } else {
        LOGI("surface callbacks registered");
      }
    } else {
      LOGI("module registration pass: no XComponent bound yet");
    }
  } else {
    LOGI(
        "module registration pass: no native XComponent object on exports yet");
  }
  return exports;
}

} // namespace

extern "C" {
static napi_module g_module = {
    .nm_version = 1,
    .nm_flags = 0,
    .nm_filename = nullptr,
    .nm_register_func = Init,
    .nm_modname = "migohost",
    .nm_priv = nullptr,
    .reserved = {nullptr},
};

__attribute__((constructor)) void RegisterMigoHostModule(void) {
  napi_module_register(&g_module);
}
}
