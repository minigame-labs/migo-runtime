//! Linux dev player for the Migo engine.
//!
//! Drives the full engine (V8 + graphics + JS game) on
//! `x86_64-unknown-linux-gnu`, either offscreen (pbuffer, no window server) or
//! in a real X11 window. The game's own telemetry (`console.error` lines)
//! proves rendering, exactly like the on-device bench harness.
//!
//! Usage:
//!   migo-player GAME_BUNDLE_DIR [SECONDS] [--window]
//!
//! GAME_BUNDLE_DIR must contain `game.json` + `game.js` (a mini-game-shaped
//! bundle). It is required so the player never depends on a developer-local
//! checkout layout.
//!
//! `--window` (or `MIGO_PLAYER_WINDOW=1`) exercises the onscreen X11 presenter.
//! Headless stays the default so CI and the PNG capture path are unaffected.

// X11 is the Linux windowed path. The offscreen path below is portable, which is
// what the PNG capture and CI use; Windows gets its window through an HWND the
// host owns, which is a separate piece of work.
#[cfg(target_os = "windows")]
mod win32_window;
#[cfg(target_os = "linux")]
mod x11_window;

use std::{path::PathBuf, sync::Arc, thread, time::Duration};

#[cfg(target_os = "windows")]
use migo_core::{HostId, host_ingress};
use migo_core::{PlatformServices, send_command_to_host, spawn_host_thread};
#[cfg(target_os = "linux")]
use platform::linux::platform::LinuxPlatform as HostPlatform;
#[cfg(target_os = "linux")]
use platform::linux::presenter::{
    LinuxOffscreenSurface as OffscreenSurface, LinuxX11Context,
    linux_graphics_platform as offscreen_graphics_platform,
};
#[cfg(target_os = "windows")]
use platform::windows::platform::WindowsPlatform as HostPlatform;
#[cfg(target_os = "windows")]
use platform::windows::presenter::{
    WindowsHwndSurface, WindowsOffscreenSurface as OffscreenSurface,
    windows_graphics_platform as offscreen_graphics_platform, windows_hwnd_graphics_platform,
};
#[cfg(target_os = "windows")]
use shared::protocol::host_cmd::{TouchData, TouchPoint, TouchType};
use shared::surface::{HostWindowMetrics, HostWindowState, PixelRatio};
use shared::{config::InitOptions, protocol::host_cmd::HostCommand, surface::SurfaceRef};

#[cfg(target_os = "windows")]
use win32_window::Win32Window;
#[cfg(target_os = "linux")]
use x11_window::X11Window;

const GAME_ID: &str = "player-demo";
const ENTRY: &str = "game.js";

/// Copy a directory tree, creating destination directories as needed.
///
/// Deliberately not following symlinks into the tree: a game bundle is content,
/// and a dev tool that dereferences a link out of it would deploy whatever the
/// link pointed at. `copy` on a symlink copies what it resolves to within this
/// process's view, which is the behaviour a bundle author expects for a file
/// they put there on purpose.
fn copy_tree(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    for entry in std::fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| format!("read entry in {}: {e}", src.display()))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|e| format!("stat {}: {e}", from.display()))?;
        if kind.is_dir() {
            std::fs::create_dir_all(&to).map_err(|e| format!("mkdir {}: {e}", to.display()))?;
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}
const SURFACE_W: u32 = 720;
const SURFACE_H: u32 = 1280;
/// How long the player waits for the first presented frame before it gives up:
/// the engine's GPU readiness budget (ten seconds) plus room to load the game.
const FIRST_PRESENT_CEILING: Duration = Duration::from_secs(30);
/// How often the window mode drains X events while the game renders.
const EVENT_POLL: Duration = Duration::from_millis(16);

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(true)
        .init();

    let mut positional = Vec::new();
    let mut windowed = std::env::var_os("MIGO_PLAYER_WINDOW").is_some_and(|v| v != "0");
    let mut resize = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage();
                return;
            }
            "--window" => windowed = true,
            "--offscreen" => windowed = false,
            "--resize" => match args.next().as_deref().and_then(parse_extent) {
                Some(extent) => resize = Some(extent),
                None => {
                    tracing::error!("--resize takes WIDTHxHEIGHT, e.g. --resize 1000x700");
                    std::process::exit(2);
                }
            },
            _ => positional.push(arg),
        }
    }
    let bundle_dir = match resolve_bundle_dir(&positional) {
        Ok(path) => path,
        Err(message) => {
            tracing::error!("{message}");
            print_usage();
            std::process::exit(2);
        }
    };
    let secs: u64 = positional.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);

    if let Err(err) = run(&bundle_dir, secs, windowed, resize) {
        tracing::error!("player failed: {err}");
        std::process::exit(1);
    }
}

fn print_usage() {
    eprintln!("usage: migo-player GAME_BUNDLE_DIR [SECONDS] [--window|--offscreen]");
}

fn resolve_bundle_dir(positional: &[String]) -> Result<PathBuf, &'static str> {
    positional
        .first()
        .filter(|arg| !arg.is_empty())
        .map(PathBuf::from)
        .ok_or("GAME_BUNDLE_DIR is required; see --help")
}

/// `WIDTHxHEIGHT`, rejecting a zero extent because no surface may carry one.
fn parse_extent(text: &str) -> Option<(u32, u32)> {
    let (width, height) = text.split_once(['x', 'X'])?;
    let width: u32 = width.trim().parse().ok()?;
    let height: u32 = height.trim().parse().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

fn run(
    bundle_dir: &PathBuf,
    secs: u64,
    windowed: bool,
    resize: Option<(u32, u32)>,
) -> Result<(), String> {
    // ---- Scratch dirs (files / cache / code cache) ----
    let root = std::env::temp_dir().join(format!("migo-player-{}", std::process::id()));
    let files_dir = root.join("files");
    let cache_dir = root.join("cache");
    let code_cache_dir = root.join("code-cache");

    // Deploy the game bundle into files_dir/migo/games/<id>/code/.
    let code_dir = files_dir
        .join("migo")
        .join("games")
        .join(GAME_ID)
        .join("code");
    std::fs::create_dir_all(&code_dir).map_err(|e| format!("mkdir code_dir: {e}"))?;
    // The whole bundle, not just its two required files.
    //
    // This used to copy exactly `game.json` and `game.js`, which meant the
    // player could not run any bundle that carried an asset -- a font, an
    // image, a sub-package. That is not a limitation of the engine, only of
    // this deployment step, and it made a whole category of conformance test
    // impossible to write: text has to ship its own typeface, because measuring
    // the platform's default face measures the platform.
    for name in ["game.json", ENTRY] {
        if !bundle_dir.join(name).is_file() {
            return Err(format!(
                "{} is not a game bundle: no {name}",
                bundle_dir.display()
            ));
        }
    }
    copy_tree(&bundle_dir, &code_dir)?;
    std::fs::create_dir_all(&cache_dir).map_err(|e| format!("mkdir cache: {e}"))?;
    std::fs::create_dir_all(&code_cache_dir).map_err(|e| format!("mkdir code-cache: {e}"))?;
    tracing::info!("deployed game '{GAME_ID}' to {}", code_dir.display());

    // ---- InitOptions (signing off: dev player has no signed receipt) ----
    let opt = InitOptions::new()
        .with_files_dir(files_dir)
        .with_cache_dir(cache_dir)
        .with_code_cache_dir(code_cache_dir)
        .with_pixel_ratio(1.0)
        .with_debug_enabled(true)
        .with_code_signing_enabled(false);

    // ---- Render target: a real X11 window, or an offscreen pbuffer ----
    // The window (when present) must outlive the engine session: the render
    // thread holds an EGL surface built from its handles, so it is dropped only
    // after the owning Host handle has been joined below.
    #[cfg(target_os = "linux")]
    let mut window = if windowed {
        Some(X11Window::open("migo-player", SURFACE_W, SURFACE_H)?)
    } else {
        None
    };
    #[cfg(target_os = "windows")]
    let mut window = if windowed {
        Some(Win32Window::open("migo-player", SURFACE_W, SURFACE_H)?)
    } else {
        None
    };
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    if windowed {
        return Err(
            "--window is implemented on Linux (X11) and Windows (HWND); \
                    this target renders offscreen"
                .to_string(),
        );
    }

    #[cfg(target_os = "linux")]
    let (surface, graphics_platform) = match window.as_ref() {
        Some(window) => {
            let (width, height) = window.size();
            let context = unsafe { LinuxX11Context::open(window.display()) }
                .map_err(|e| format!("open owned X11 render context: {e:?}"))?;
            let surface = context.surface(window.window(), width, height);
            let platform = context.graphics_platform();
            (surface, platform)
        }
        None => {
            let surface: SurfaceRef = Arc::new(OffscreenSurface::new(SURFACE_W, SURFACE_H));
            let platform =
                offscreen_graphics_platform().map_err(|e| format!("graphics platform: {e:?}"))?;
            (surface, platform)
        }
    };
    #[cfg(target_os = "windows")]
    let (surface, graphics_platform) = match window.as_ref() {
        Some(window) => {
            let (width, height) = window.size();
            // SAFETY: the window outlives the session -- it is dropped only
            // after the Host join below, by which point the render thread has
            // let go of the EGL surface built from this handle.
            let surface: SurfaceRef =
                Arc::new(unsafe { WindowsHwndSurface::new(window.hwnd(), width, height) });
            let platform = windows_hwnd_graphics_platform()
                .map_err(|e| format!("hwnd graphics platform: {e:?}"))?;
            (surface, platform)
        }
        None => {
            let surface: SurfaceRef = Arc::new(OffscreenSurface::new(SURFACE_W, SURFACE_H));
            let platform =
                offscreen_graphics_platform().map_err(|e| format!("graphics platform: {e:?}"))?;
            (surface, platform)
        }
    };
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    let (surface, graphics_platform) = {
        let surface: SurfaceRef = Arc::new(OffscreenSurface::new(SURFACE_W, SURFACE_H));
        let platform =
            offscreen_graphics_platform().map_err(|e| format!("graphics platform: {e:?}"))?;
        (surface, platform)
    };
    let mode = if windowed { "window" } else { "offscreen" };
    // The requested size is not always what the surface got: a window manager
    // may clamp a frame that does not fit the display, and the engine renders
    // into the client area it actually has. Logging the constants instead of the
    // real extent made the line disagree with the pixels captured from it.
    let (surface_w, surface_h) = surface.size();

    // Tell the platform what content should see from `migo.getSystemInfoSync()`.
    //
    // Read from the surface rather than the requested constants for the same
    // reason the log line above is: content laying itself out from a size the
    // window manager refused is content laying itself out wrong. The player has
    // no HiDPI notion, so one physical pixel is one CSS pixel.
    // Held by the player as well as the platform: the host owns this state and
    // republishes it whenever the window it presents into changes size, which is
    // what lets `migo.getSystemInfoSync()` follow a resize instead of reporting
    // the size the window had at start-up.
    let window_state = Arc::new(HostWindowState::new(HostWindowMetrics::new(
        surface_w,
        surface_h,
        PixelRatio::new(1.0).expect("1.0 is a valid pixel ratio"),
    )));
    let host_kit: Arc<dyn PlatformServices> =
        Arc::new(HostPlatform::new().with_window(Arc::clone(&window_state)));
    tracing::info!("spawning host thread ({surface_w}x{surface_h} {mode})");
    // The player always has its window before it starts, so this is never the
    // warm start `spawn_host_thread` also accepts.
    let host = spawn_host_thread(Some(surface), graphics_platform, host_kit, opt)
        .map_err(|e| format!("spawn_host_thread: {e:?}"))?;
    let host_id = host.id();
    tracing::info!("host {host_id} spawned; loading game");

    send_command_to_host(
        host_id,
        HostCommand::EvaluateModule {
            game_id: GAME_ID.to_string(),
            entry: ENTRY.to_string(),
        },
    )
    .map_err(|e| format!("EvaluateModule: {e}"))?;

    // Capture the FIRST presented frame to PNG (proves the offscreen render
    // visually). Requested immediately so the render thread grabs an early
    // frame during the initial active-render burst — robust against the game
    // later going idle/erroring. The render thread fills FBO 0 (after the
    // DrawingBuffer blit, before eglSwapBuffers) exactly once.
    // Defaults into the run's scratch dir, not the working tree: running the
    // player from the repo root should not leave an untracked PNG behind.
    let png_path = std::env::var_os("MIGO_PLAYER_PNG")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("migo-player-frame.png"));
    graphics::frame_capture::request();
    // The run is timed from the first presented frame, not from launch: start-up
    // is not bounded by the run (the engine gives the GPU up to ten seconds), and
    // a window that included it measured start-up whenever start-up was slow --
    // a cold software renderer on a loaded runner took four seconds to come up,
    // and a presentation probe with the engine working painted nothing. The
    // ceiling is for an engine that never presents, which is then said outright.
    if !graphics::frame_capture::wait_for_present(FIRST_PRESENT_CEILING) {
        return Err(format!(
            "nothing was presented within {}s of loading the game",
            FIRST_PRESENT_CEILING.as_secs()
        ));
    }
    // Let the game render for the window; the render thread keeps overwriting
    // the capture slot with the latest present, so early blank warmup frames
    // are superseded by frames containing game content.
    let total = Duration::from_secs(secs.max(4));
    match resize {
        // Offscreen is the only mode that can resize itself: a pbuffer of the
        // new size *is* the new surface, whereas a window's extent belongs to
        // the window system. Splitting the run in half puts frames on both
        // sides of the transition, so a capture proves which size was presented
        // rather than only which size was requested.
        Some((width, height)) if !windowed => {
            thread::sleep(total / 2);
            resize_offscreen(host_id, &window_state, width, height)?;
            // Ask for a second capture: the stored frame must be one presented
            // *after* the transition, or the PNG would prove nothing about it.
            graphics::frame_capture::request();
            thread::sleep(total / 2);
        }
        Some(_) => {
            return Err(
                "--resize needs --offscreen; a window's extent is the window system's to change"
                    .to_string(),
            );
        }
        None => {
            #[cfg(target_os = "linux")]
            run_for(&mut window, total);
            #[cfg(target_os = "windows")]
            run_for(&mut window, total, host_id);
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            run_for(total);
        }
    }
    match graphics::frame_capture::take() {
        Some(frame) => {
            // With `--resize` the capture is the acceptance evidence, so the
            // frame has to be one the *resized* surface presented. Writing a
            // frame of any other extent would report a resize that never
            // happened as a pass, which is the whole failure this run exists to
            // detect.
            if let Some((width, height)) = resize {
                if (frame.width, frame.height) != (width, height) {
                    return Err(format!(
                        "resized to {width}x{height} but the presented frame is {}x{}",
                        frame.width, frame.height
                    ));
                }
            }
            write_png(&png_path, &frame)?;
            tracing::info!(
                "captured {}x{} frame -> {}",
                frame.width,
                frame.height,
                png_path.display()
            );
        }
        None if resize.is_some() => {
            return Err(
                "nothing was presented after the resize, so no frame proves it took effect"
                    .to_string(),
            );
        }
        None => tracing::warn!("frame capture: no frame was presented during the window"),
    }

    report_draw_batching(host_id);

    tracing::info!("shutting down host {host_id}");
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    shutdown_before_drop(host, window)?;
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    shutdown_before_drop(host, ())?;
    tracing::info!("player done");
    Ok(())
}

/// Print the draw-call batching headroom for the run that just finished.
///
/// **The measurement the batching question was missing.** `GlBatch` executes its
/// commands in order and merges nothing, and whether that costs anything was
/// never measured — merging is largely the application's job in WebGL, since
/// state can change between draws, so the answer depends entirely on how the
/// game actually draws.
///
/// `adjacent_draws` counts draws issued with zero *post-dedup* state changes
/// since the previous draw of the same frame, which is the upper bound on what a
/// batching pass could merge. Post-dedup matters: a state command the shadow
/// swallowed never reached the driver, so draws separated only by those are
/// adjacent as far as the GPU is concerned.
///
/// A ratio near zero closes the question for the workload measured. A high one
/// does not open it on its own — a merge also needs matching primitive modes and
/// contiguous vertex or index ranges, which the counter does not check.
///
/// Printed unconditionally rather than behind a flag: it is three integers at
/// shutdown, the pixel gates read a captured PNG rather than stdout, and a
/// number nobody can see is how this went unmeasured in the first place.
fn report_draw_batching(host_id: i32) {
    let Some(stats) = shared::stats::get_stats(host_id) else {
        tracing::warn!("draw batching: no stats for host {host_id}");
        return;
    };
    use std::sync::atomic::Ordering;
    let draws = stats.draw_calls.load(Ordering::Relaxed);
    let changes = stats.state_changes.load(Ordering::Relaxed);
    let adjacent = stats.adjacent_draws.load(Ordering::Relaxed);
    let mergeable = stats.mergeable_draws.load(Ordering::Relaxed);

    if draws == 0 {
        println!("[draw-batching] no draws were dispatched");
        return;
    }
    let pct = |n: u32| f64::from(n) * 100.0 / f64::from(draws);
    println!(
        "[draw-batching] draws={draws} state_changes={changes} \
         ({:.2} per draw) adjacent={adjacent} ({:.1}%) \
         mergeable={mergeable} ({:.1}% could become one draw without \
         changing a pixel)",
        f64::from(changes) / f64::from(draws),
        pct(adjacent),
        pct(mergeable),
    );
}

/// Report a new offscreen surface the way an embedding host reports a resize.
///
/// This is the whole sequence a host owes the engine, in the order the C ABI
/// performs it: build the new native surface, lease it against the host's live
/// Surface generation, publish the measurement content reads, then hand the
/// lease over. The publish happens before the command is enqueued and is rolled
/// back if the enqueue fails, so `migo.getSystemInfoSync()` can never report a
/// size the renderer was never told about.
///
/// `UpdateSurface` goes through the critical channel because a dropped resize
/// strands the app on a stale-sized frame with nothing left to trigger another.
fn resize_offscreen(
    host_id: migo_core::HostId,
    window_state: &Arc<HostWindowState>,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let pixel_ratio = PixelRatio::new(1.0).expect("1.0 is a valid pixel ratio");
    let surface: SurfaceRef = Arc::new(OffscreenSurface::new(width, height));
    let lease = migo_core::lease_surface(host_id, surface)
        .map_err(|error| format!("lease resized surface: {error}"))?;

    let previous = window_state.replace(HostWindowMetrics::new(width, height, pixel_ratio));
    if let Err(error) = migo_core::send_critical_command_to_host(
        host_id,
        HostCommand::UpdateSurface {
            lease,
            pixel_ratio: Some(pixel_ratio),
        },
    ) {
        window_state.replace(previous);
        return Err(format!("UpdateSurface: {error}"));
    }
    tracing::info!("resized offscreen surface to {width}x{height}");
    Ok(())
}

fn shutdown_before_drop<T>(
    mut host: migo_core::HostThread,
    native_resource: T,
) -> Result<(), String> {
    let result = host
        .shutdown_and_join()
        .map_err(|error| format!("shutdown_and_join: {error}"));
    // A failed shutdown request leaves ownership intact. Dropping the owner
    // still performs the synchronous fail-safe join before native teardown.
    drop(host);
    drop(native_resource);
    result
}

/// Wait for `total`, servicing window events when running windowed.
///
/// Returns early if the window manager asks the window to close, so the caller
/// still runs its capture + shutdown path instead of being killed mid-frame.
#[cfg(target_os = "linux")]
fn run_for(window: &mut Option<X11Window>, total: Duration) {
    let Some(window) = window.as_mut() else {
        thread::sleep(total);
        return;
    };
    let deadline = std::time::Instant::now() + total;
    while std::time::Instant::now() < deadline {
        if !window.pump() {
            tracing::info!("window close requested; stopping early");
            return;
        }
        thread::sleep(EVENT_POLL);
    }
}

/// Same contract as the Linux one, over the Win32 message queue.
#[cfg(target_os = "windows")]
fn run_for(window: &mut Option<Win32Window>, total: Duration, host_id: HostId) {
    let Some(window) = window.as_mut() else {
        thread::sleep(total);
        return;
    };
    // Acquired once, not per event. `send_command_to_host` takes a read lock on
    // the global host map and clones the sender on every call; a mouse move can
    // fire hundreds of times a second, so paying that per event would put a
    // lock acquisition and an Arc clone on the input hot path. The ingress
    // handle holds the sender directly, which is also how the C ABI delivers
    // input.
    let mut pointer = match host_ingress(host_id) {
        Ok(ingress) => Some(PointerState::new(ingress)),
        Err(e) => {
            // Rendering still proves out without input, so this degrades rather
            // than aborts -- but it is said plainly, because a window that
            // silently ignores clicks looks exactly like content that does.
            tracing::warn!("no input: host ingress unavailable ({e})");
            None
        }
    };
    let deadline = std::time::Instant::now() + total;
    while std::time::Instant::now() < deadline {
        if !window.pump() {
            tracing::info!("window close requested; stopping early");
            return;
        }
        if let Some(pointer) = pointer.as_mut() {
            for event in window.drain_pointer() {
                pointer.deliver(event);
            }
        }
        thread::sleep(EVENT_POLL);
    }
}

/// Translates what the window saw into the two streams content may listen on.
///
/// Deliberately the same shape the Qt X11 view uses on Linux: a desktop host
/// sends the mouse stream, and also maps the mouse to a single finger so that
/// touch-only content -- which is nearly all mini-game content -- responds at all.
///
/// One physical click therefore reaches BOTH `onMouseDown` and `onTouchStart`.
/// That matches Linux, but content listening on both (rare on mini-game platforms, common in
/// HTML5) acts on one press twice. The web platform avoids this by firing
/// compatibility mouse events only after a touch sequence ends and letting
/// `preventDefault` suppress them; this runtime has no such suppression, and
/// adding it belongs at the engine level rather than being solved differently in
/// every host.
///
/// Nothing here allocates per event: the touch batch goes through the
/// preallocated pool, and the points array is reused in place.
#[cfg(target_os = "windows")]
struct PointerState {
    ingress: migo_core::HostIngress,
    buttons_held: u32,
    points: [TouchPoint; 10],
}

#[cfg(target_os = "windows")]
impl PointerState {
    fn new(ingress: migo_core::HostIngress) -> Self {
        Self {
            ingress,
            buttons_held: 0,
            points: [TouchPoint::default(); 10],
        }
    }

    fn deliver(&mut self, event: win32_window::PointerEvent) {
        use win32_window::PointerEvent;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0);

        match event {
            PointerEvent::Down { x, y, button } => {
                let first = self.buttons_held == 0;
                self.buttons_held |= 1 << button;
                self.send(HostCommand::OnMouseDown {
                    x,
                    y,
                    button,
                    timestamp_ms: now,
                });
                if first {
                    self.send_touch(TouchType::Start, x, y, now);
                }
            }
            PointerEvent::Move { x, y } => {
                self.send(HostCommand::OnMouseMove {
                    x,
                    y,
                    button: 0,
                    timestamp_ms: now,
                });
                // Touch has no hover: a finger that is not down cannot move, so
                // free motion would be events no game reads.
                if self.buttons_held != 0 {
                    self.send_touch(TouchType::Move, x, y, now);
                }
            }
            PointerEvent::Up { x, y, button } => {
                self.buttons_held &= !(1 << button);
                self.send(HostCommand::OnMouseUp {
                    x,
                    y,
                    button,
                    timestamp_ms: now,
                });
                // The finger lifts when the LAST button does: ending on the
                // first release would strand a drag another button still holds.
                if self.buttons_held == 0 {
                    self.send_touch(TouchType::End, x, y, now);
                }
            }
        }
    }

    fn send(&self, cmd: HostCommand) {
        // A dropped input event is not worth ending the session over, but it is
        // worth saying: silence looks identical to content ignoring input.
        if let Err(e) = self.ingress.try_send(cmd) {
            tracing::warn!("input not delivered: {e:?}");
        }
    }

    fn send_touch(&mut self, kind: TouchType, x: f32, y: f32, now: f64) {
        let ending = matches!(kind, TouchType::End | TouchType::Cancel);
        self.points[0] = TouchPoint {
            id: 0,
            x,
            y,
            pressure: if ending { 0.0 } else { 1.0 },
            // bit 0 = in changedTouches, bit 1 = removed from the surface.
            flags: if ending { 0b11 } else { 0b01 },
        };
        if let Err(e) = self.ingress.try_send_touch(TouchData {
            touch_type: kind,
            count: 1,
            points: self.points,
            timestamp_ms: now as i64,
        }) {
            tracing::warn!("touch not delivered: {e:?}");
        }
    }
}

/// Offscreen has no event source to service, so waiting is the whole job.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn run_for(total: Duration) {
    thread::sleep(total);
}

/// Flip GL bottom-up rows to top-down and encode an RGBA8 PNG.
fn write_png(
    path: &std::path::Path,
    frame: &graphics::frame_capture::CapturedFrame,
) -> Result<(), String> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let stride = w * 4;
    if frame.rgba_bottom_up.len() != stride * h {
        return Err(format!(
            "capture size mismatch: {} != {}x{}x4",
            frame.rgba_bottom_up.len(),
            w,
            h
        ));
    }
    let mut top_down = vec![0u8; frame.rgba_bottom_up.len()];
    for y in 0..h {
        let src = &frame.rgba_bottom_up[(h - 1 - y) * stride..(h - y) * stride];
        top_down[y * stride..(y + 1) * stride].copy_from_slice(src);
    }
    let file =
        std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("png header: {e}"))?;
    writer
        .write_image_data(&top_down)
        .map_err(|e| format!("png data: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        thread,
    };

    use super::{resolve_bundle_dir, shutdown_before_drop};

    #[test]
    fn player_requires_explicit_bundle_directory() {
        assert_eq!(
            resolve_bundle_dir(&[]),
            Err("GAME_BUNDLE_DIR is required; see --help")
        );
    }

    struct DropRecorder {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Drop for DropRecorder {
        fn drop(&mut self) {
            self.events.lock().expect("event log").push("window");
        }
    }

    #[test]
    fn teardown_joins_host_before_dropping_native_resource() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let host_events = Arc::clone(&events);
        let join = thread::Builder::new()
            .name("Migo-Main-player-teardown".to_owned())
            .spawn(move || {
                host_events.lock().expect("event log").push("host");
            })
            .expect("spawn test Host");
        let host = migo_core::HostThread::from_join_handle_for_test(9_001, join);

        shutdown_before_drop(
            host,
            DropRecorder {
                events: Arc::clone(&events),
            },
        )
        .expect("teardown");

        assert_eq!(*events.lock().expect("event log"), ["host", "window"]);
    }

    #[test]
    fn teardown_drops_native_resource_only_after_observing_host_panic() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let host_events = Arc::clone(&events);
        let join = thread::Builder::new()
            .name("Migo-Main-player-panic".to_owned())
            .spawn(move || {
                host_events.lock().expect("event log").push("host");
                panic!("test Host panic");
            })
            .expect("spawn test Host");
        let host = migo_core::HostThread::from_join_handle_for_test(9_002, join);

        let result = shutdown_before_drop(
            host,
            DropRecorder {
                events: Arc::clone(&events),
            },
        );

        assert!(result.is_err());
        assert_eq!(*events.lock().expect("event log"), ["host", "window"]);
    }
}
