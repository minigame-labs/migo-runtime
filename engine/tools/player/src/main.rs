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
//!
//! `--network-policy FILE` gives the game the network a host would: FILE is the
//! JSON a C-ABI host writes into `MigoSessionConfig.network_policy_json_utf8`.
//! Without it the game reaches no network at all, as under a host that set none.

// X11 is the Linux windowed path. The offscreen path below is portable, which is
// what the PNG capture and CI use; Windows gets its window through an HWND the
// host owns, which is a separate piece of work.
#[cfg(target_os = "windows")]
mod win32_window;
#[cfg(target_os = "linux")]
mod x11_window;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "windows")]
use migo_core::{HostId, host_ingress};
use migo_core::{HostNotifier, PlatformServices, send_command_to_host, spawn_host_thread};
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
/// The player's own id, as a mobile host's package name is its own: content reads it as `host.appId`.
const PLAYER_APP_ID: &str = "com.migo.player";
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
/// How long the player waits for the first presented frame before it starts the
/// run without one: the engine's GPU readiness budget (ten seconds) plus room to
/// load the game.
const FIRST_PRESENT_CEILING: Duration = Duration::from_secs(30);
/// How often the window mode drains X events while the game renders.
const EVENT_POLL: Duration = Duration::from_millis(16);
/// How often the wait for the first frame looks at whether the launch has failed.
const START_POLL: Duration = Duration::from_millis(50);

/// What the engine tells the player about the launch: whether the game failed to start.
///
/// The player heard nothing from the engine, so a game whose entry threw ran out the whole
/// present ceiling and the run after it, then said only that nothing was presented -- the reason
/// was in the engine's log, and a launch the content table does not cover (a renderer that never
/// came up) was not in the report at all.
#[derive(Default)]
struct Launch {
    failure: Mutex<Option<String>>,
}

impl HostNotifier for Launch {
    fn notify_game_failed(&self, _host_id: i32, error_code: u16, message: &str, detail: &str) {
        let reason = if detail.is_empty() {
            format!("{message} (engine code {error_code})")
        } else {
            format!("{message}: {detail} (engine code {error_code})")
        };
        *self.failure.lock().expect("launch outcome") = Some(reason);
    }
}

impl Launch {
    /// The launch's failure, as the run's error.
    fn failed(&self) -> Result<(), String> {
        match self.failure.lock().expect("launch outcome").as_ref() {
            Some(reason) => Err(format!("the game did not start: {reason}")),
            None => Ok(()),
        }
    }

    /// Wait for the first presented frame, at most `ceiling`, and end the wait when the launch
    /// fails: a game that did not start presents nothing, however long it is given. Whether a
    /// frame was presented.
    fn wait_for_start(&self, ceiling: Duration) -> Result<bool, String> {
        let deadline = Instant::now() + ceiling;
        loop {
            self.failed()?;
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(false);
            }
            if graphics::frame_capture::wait_for_present(left.min(START_POLL)) {
                return Ok(true);
            }
        }
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(true)
        .init();

    let mut positional = Vec::new();
    let mut windowed = std::env::var_os("MIGO_PLAYER_WINDOW").is_some_and(|v| v != "0");
    let mut resize = None;
    let mut network_policy = shared::network_policy::NetworkPolicy::default();
    let mut prelude_files = Vec::new();
    let mut sessions: u32 = 1;
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
            "--network-policy" => {
                let read = args
                    .next()
                    .ok_or_else(|| "--network-policy takes a JSON file".to_string())
                    .and_then(|file| {
                        std::fs::read_to_string(&file).map_err(|error| format!("{file}: {error}"))
                    })
                    .and_then(|json| {
                        shared::network_policy::NetworkPolicy::from_json(&json)
                            .map_err(|error| error.to_string())
                    });
                match read {
                    Ok(policy) => network_policy = policy,
                    Err(message) => {
                        tracing::error!("--network-policy: {message}");
                        std::process::exit(2);
                    }
                }
            }
            "--sessions" => match args
                .next()
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|n| *n > 0)
            {
                Some(n) => sessions = n,
                None => {
                    tracing::error!("--sessions takes a count of at least 1");
                    std::process::exit(2);
                }
            },
            "--prelude" => match args.next() {
                Some(file) => prelude_files.push(PathBuf::from(file)),
                None => {
                    tracing::error!("--prelude takes a script file");
                    std::process::exit(2);
                }
            },
            _ => positional.push(arg),
        }
    }
    let preludes = match prelude_scripts(&prelude_files) {
        Ok(scripts) => scripts,
        Err(message) => {
            tracing::error!("--prelude: {message}");
            std::process::exit(2);
        }
    };
    let bundle_dir = match resolve_bundle_dir(&positional) {
        Ok(path) => path,
        Err(message) => {
            tracing::error!("{message}");
            print_usage();
            std::process::exit(2);
        }
    };
    let secs: u64 = positional.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);

    if sessions > 1 {
        if windowed || resize.is_some() {
            tracing::error!(
                "--sessions runs offscreen, without --resize: what it measures is the sessions"
            );
            std::process::exit(2);
        }
        if let Err(err) = run_sessions(&bundle_dir, secs, sessions, network_policy, preludes) {
            tracing::error!("player failed: {err}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(err) = run(
        &bundle_dir,
        secs,
        windowed,
        resize,
        network_policy,
        preludes,
    ) {
        tracing::error!("player failed: {err}");
        std::process::exit(1);
    }
}

fn print_usage() {
    eprintln!(
        "usage: migo-player GAME_BUNDLE_DIR [SECONDS] [--window|--offscreen] [--network-policy FILE] \
         [--prelude SCRIPT]... [--sessions N]"
    );
}

/// The boot preludes `--prelude` names, in the order given: each runs in the global
/// scope before the game's entry, as a host injects an adapter. Content written
/// against `wx` or the browser runs here only with its adapter in front of it.
fn prelude_scripts(files: &[PathBuf]) -> Result<Vec<(String, String)>, String> {
    files
        .iter()
        .map(|file| {
            let source =
                std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
            let name = file
                .file_name()
                .map(|n| format!("<{}>", n.to_string_lossy()))
                .ok_or_else(|| format!("{} names no file", file.display()))?;
            Ok((name, source))
        })
        .collect()
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

/// Deploy the bundle into a scratch app directory and build the options a session starts
/// from. Every session of a run starts from these, so what one leaves in the app's files or
/// caches the next finds, as it would in an app.
fn prepare(
    bundle_dir: &PathBuf,
    network_policy: shared::network_policy::NetworkPolicy,
    preludes: Vec<(String, String)>,
) -> Result<(PathBuf, InitOptions), String> {
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
        .with_code_signing_enabled(false)
        .with_network_policy(network_policy)
        .with_prelude_scripts(preludes);

    Ok((root, opt))
}

fn run(
    bundle_dir: &PathBuf,
    secs: u64,
    windowed: bool,
    resize: Option<(u32, u32)>,
    network_policy: shared::network_policy::NetworkPolicy,
    preludes: Vec<(String, String)>,
) -> Result<(), String> {
    let (root, opt) = prepare(bundle_dir, network_policy, preludes)?;

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
    let launch = Arc::new(Launch::default());
    let host_kit: Arc<dyn PlatformServices> = Arc::new(
        HostPlatform::new()
            .with_window(Arc::clone(&window_state))
            .with_host_app(shared::services::app_info::HostAppInfo {
                host_app_id: Some(PLAYER_APP_ID.to_string()),
                ..Default::default()
            })
            .with_notifier(launch.clone()),
    );
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
    // and a presentation probe with the engine working painted nothing.
    //
    // Content that has presented nothing by the ceiling still runs: a game may
    // work offscreen for a long time before its first frame, and some content
    // never presents at all (a conformance bundle that draws only offscreen
    // canvases). Ending the run here cut such content off mid-work, on a slow
    // software renderer, while the same content ran to completion through the C
    // ABI host. Its run is timed from the ceiling instead, and an engine that
    // never presents is said outright when the run ends without a frame. A game
    // that did not start is not one of those, and ends the run when it says so.
    if !launch.wait_for_start(FIRST_PRESENT_CEILING)? {
        tracing::warn!(
            "nothing presented within {}s of loading the game; running on without a frame",
            FIRST_PRESENT_CEILING.as_secs()
        );
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
    // A launch can fail after its loading screen presented: the frame would read as a game
    // that ran.
    launch.failed()?;
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
        None => return Err("nothing was presented during the run".to_string()),
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

/// Run the game `sessions` times in one process, each in a session of its own, and report what
/// the process holds after each session is shut down and joined (see [`left_behind`]).
///
/// What a game centre does many times a day: open a game, play, close it, open the next. A
/// session that leaves anything behind -- a canvas, an isolate, a thread, a texture -- grows the
/// process by that much each time, which one long session never shows. Each session must present
/// a frame, or the run fails: sessions that never started would also leave nothing behind.
fn run_sessions(
    bundle_dir: &PathBuf,
    secs: u64,
    sessions: u32,
    network_policy: shared::network_policy::NetworkPolicy,
    preludes: Vec<(String, String)>,
) -> Result<(), String> {
    let (root, opt) = prepare(bundle_dir, network_policy, preludes)?;
    let png_path = std::env::var_os("MIGO_PLAYER_PNG")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("migo-player-frame.png"));
    for index in 1..=sessions {
        let surface: SurfaceRef = Arc::new(OffscreenSurface::new(SURFACE_W, SURFACE_H));
        let graphics_platform =
            offscreen_graphics_platform().map_err(|e| format!("graphics platform: {e:?}"))?;
        let (surface_w, surface_h) = surface.size();
        let window_state = Arc::new(HostWindowState::new(HostWindowMetrics::new(
            surface_w,
            surface_h,
            PixelRatio::new(1.0).expect("1.0 is a valid pixel ratio"),
        )));
        let launch = Arc::new(Launch::default());
        let host_kit: Arc<dyn PlatformServices> = Arc::new(
            HostPlatform::new()
                .with_window(Arc::clone(&window_state))
                .with_host_app(shared::services::app_info::HostAppInfo {
                    host_app_id: Some(PLAYER_APP_ID.to_string()),
                    ..Default::default()
                })
                .with_notifier(launch.clone()),
        );
        let host = spawn_host_thread(Some(surface), graphics_platform, host_kit, opt.clone())
            .map_err(|e| format!("session {index}: spawn_host_thread: {e:?}"))?;
        let host_id = host.id();
        graphics::frame_capture::request();
        let started = send_command_to_host(
            host_id,
            HostCommand::EvaluateModule {
                game_id: GAME_ID.to_string(),
                entry: ENTRY.to_string(),
            },
        )
        .map_err(|e| format!("session {index}: EvaluateModule: {e}"))
        .and_then(|()| {
            launch
                .wait_for_start(FIRST_PRESENT_CEILING)
                .map_err(|e| format!("session {index}: {e}"))
        });
        if started == Ok(true) {
            thread::sleep(Duration::from_secs(secs.max(1)));
        }
        let frame = graphics::frame_capture::take();
        shutdown_before_drop(host, ())?;
        started?;
        launch
            .failed()
            .map_err(|e| format!("session {index}: {e}"))?;
        let frame = frame.ok_or_else(|| format!("session {index} presented nothing"))?;
        if index == sessions {
            write_png(&png_path, &frame)?;
        }
        drop(frame);
        let (heap, resident) = left_behind()?;
        tracing::info!(
            "session {index}/{sessions}: heap {heap} kB in use, resident {resident} kB, after shutdown"
        );
    }
    Ok(())
}

/// What a shut-down session left in the process: the C heap's live bytes, and the resident set
/// once the heap has handed back what it holds free.
///
/// The resident set as it stands says little. glibc keeps what a session freed in the arenas its
/// threads used, so sixty sessions grew it from 160 to 467 MB with nothing left alive, and the
/// next sixty levelled off at 340. Trimmed first, it stays where the first session put it unless
/// something is still held: a V8 heap, a thread's stack, a driver's mapping. The heap's live
/// bytes see what the resident set is too coarse for -- the GL driver reloaded every session
/// left 4 kB each, a thousandth of it.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn left_behind() -> Result<(u64, u64), String> {
    /// glibc's `struct mallinfo2` (glibc 2.33), summed over every arena. All of it is glibc's
    /// layout; two fields are read.
    #[repr(C)]
    #[allow(dead_code)]
    struct MallInfo2 {
        arena: usize,
        ordblks: usize,
        smblks: usize,
        hblks: usize,
        hblkhd: usize,
        usmblks: usize,
        fsmblks: usize,
        uordblks: usize,
        fordblks: usize,
        keepcost: usize,
    }
    unsafe extern "C" {
        fn mallinfo2() -> MallInfo2;
        fn malloc_trim(pad: usize) -> i32;
    }
    // SAFETY: neither takes a pointer, and glibc locks each arena while it reads or trims it.
    let info = unsafe { mallinfo2() };
    // SAFETY: as above.
    unsafe { malloc_trim(0) };
    let heap = (info.uordblks + info.hblkhd) as u64 / 1024;
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|e| format!("/proc/self/status: {e}"))?;
    let resident = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.trim().trim_end_matches("kB").trim().parse().ok())
        .ok_or_else(|| "no VmRSS in /proc/self/status".to_string())?;
    Ok((heap, resident))
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn left_behind() -> Result<(u64, u64), String> {
    Err("--sessions reads glibc's heap and /proc, which only a Linux glibc build has".to_string())
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

    use super::{prelude_scripts, resolve_bundle_dir, shutdown_before_drop};

    #[test]
    fn preludes_keep_their_order_and_are_named_for_their_files() {
        let dir = std::env::temp_dir().join(format!("migo-player-preludes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("adapter.js");
        let second = dir.join("recorder.js");
        std::fs::write(&first, "globalThis.a = 1;").unwrap();
        std::fs::write(&second, "globalThis.b = 2;").unwrap();
        let scripts = prelude_scripts(&[second.clone(), first.clone()]).unwrap();
        assert_eq!(
            scripts,
            vec![
                ("<recorder.js>".to_string(), "globalThis.b = 2;".to_string()),
                ("<adapter.js>".to_string(), "globalThis.a = 1;".to_string()),
            ]
        );
        let missing = dir.join("missing.js");
        let error = prelude_scripts(&[first, missing.clone()]).unwrap_err();
        assert!(error.starts_with(&missing.display().to_string()), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
