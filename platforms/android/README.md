# Migo Runtime SDK for Android

[English](README.md) | [中文](README.zh-CN.md)

A lightweight, high-performance JavaScript game runtime for Android.

## Features

- 🚀 **High Performance** - Native Rust engine with OpenGL ES rendering
- 📱 **API 26+** - Supports Android 8.0 Oreo and above
- 🔧 **Zero Dependencies** - No AndroidX, Kotlin, or third-party libraries required
- 🎮 **Game Ready** - Canvas 2D and WebGL support
- 🔒 **Sandboxed Filesystem** - Isolated file storage per game

## Installation

Release builds have two products: `slim` keeps the base capability set —
rendering, input, lifecycle, storage/VFS, network, subpackages, and host
messaging — plus code signing and V8 resource limits; `full` adds sensors,
extended media, connectivity, commerce/payment, and system capability groups
on top of that base. Both enforce API 26.

```bash
bash scripts/build-aar.sh --product-profile full release
bash scripts/build-aar.sh --product-profile slim release
```

### Gradle

Add the AAR to your project:

```groovy
dependencies {
    implementation files('libs/migo-<version>-android.aar')
}
```

The AAR carries both `arm64-v8a` and `x86_64` (34 MB). **A shipped app carries one
ABI**: add

```groovy
android {
    defaultConfig {
        ndk { abiFilters 'arm64-v8a' }
    }
}
```

and the packaged native library is 17 MB. Publish an App Bundle instead and Play
delivers per device, so `x86_64` contributes nothing to the end-user download.

There is no Maven repository yet; use the local AAR.

## Shipping the engine on demand

`libmigo.so` is about 17 MB of store download and 45 MB installed, per ABI. If a
mini-game is a secondary feature of your app, you can ship an APK without it and
fetch it the first time a user opens a game — users who never do never pay for it.

Depend on `migo-<version>-android-nojni.aar` instead of `migo-<version>-android.aar`,
take the engine from `migo-<version>-jni-android-<arch>.tar.gz`, and hand it over:

```java
MigoNativeLoader.setProvider(context, abi -> {
    File engine = new File(context.getNoBackupFilesDir(), abi + "/libmigo.so");
    return engine.isFile() ? engine : null;   // null means "not downloaded yet"
});
```

The file is verified against the artifact manifest embedded in the AAR before it
is loaded, so a partial download or a mirror serving the previous release fails
with a readable reason instead of crashing inside the engine.
`MigoNativeLoader.requiredArtifact(context)` returns the digest to check against,
and `MigoNativeLoader.prepare(context, file)` runs that check on the thread you
call it from — so your download code learns about a bad file immediately rather
than a user meeting it as a launch failure later.

Where you may fetch it from depends on your store: on Google Play the only
compliant source is [Play Feature Delivery](https://developer.android.com/guide/playcore/feature-delivery)
(fetching executable code from anywhere else violates the Device and Network Abuse
policy); stores without Feature Delivery expect you to host the file yourself,
which [LEGAL.md](../../LEGAL.md) confirms is permitted. Migo never downloads anything
itself, because one built-in downloader would be wrong for one of the two.

## Quick Start

### Basic Usage

```java
import com.migo.runtime.MigoRuntime;
import com.migo.runtime.GameSession;
import com.migo.runtime.RuntimeConfig;

public class GameActivity extends Activity {
    private GameSession session;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        
        // 1. Create a SurfaceView for rendering
        SurfaceView surfaceView = new SurfaceView(this);
        setContentView(surfaceView);

        // 2. Configure the runtime
        RuntimeConfig config = new RuntimeConfig.Builder(this)
            .setDebugEnabled(BuildConfig.DEBUG)
            .setLogLevel(RuntimeConfig.LogLevel.DEBUG)
            .setTargetFps(60)
            .build();

        // 3. Set up surface callbacks
        surfaceView.getHolder().addCallback(new SurfaceHolder.Callback() {
            @Override
            public void surfaceCreated(SurfaceHolder holder) {
                // 4. Create game session (each game needs a unique gameId)
                session = MigoRuntime.getInstance()
                    .createSession(GameActivity.this, holder.getSurface(), config, "my-game-id");
                
                // 5. Start the game (loads from isolated code directory)
                // Place game code in: session.getPaths().getCodeDir()
                session.startGame("game.js");
            }

            @Override
            public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
                if (session != null) {
                    session.updateSurface(holder.getSurface(), width, height);
                }
            }

            @Override
            public void surfaceDestroyed(SurfaceHolder holder) {
                if (session != null) {
                    session.onSurfaceDestroyed();
                }
            }
        });

        // 6. Handle touch events
        surfaceView.setOnTouchListener((v, event) -> {
            if (session != null) {
                return session.dispatchTouchEvent(event);
            }
            return false;
        });
    }

    @Override
    protected void onPause() {
        super.onPause();
        if (session != null) session.pause();
    }

    @Override
    protected void onResume() {
        super.onResume();
        if (session != null) session.resume();
    }

    @Override
    protected void onDestroy() {
        if (session != null) {
            session.close();
            session = null;
        }
        super.onDestroy();
    }
}
```

### With Event Callbacks

```java
// Set up unified session listener
session.setListener(new GameSessionListener() {
    @Override
    public void onGameReady() {
        Log.i(TAG, "Game is ready!");
        hideLoadingScreen();
    }

    @Override
    public void onGameExit(int exitCode) {
        Log.i(TAG, "Game exited with code: " + exitCode);
        finish();
    }

    @Override
    public void onError(int errorCode, String message, boolean recoverable) {
        Log.e(TAG, "Error " + errorCode + ": " + message);
        if (!recoverable) {
            showErrorDialog(message);
        }
    }

    // Optional: override onLoadingProgress / onPaused / onResumed etc.
});
```

### Safe Session Creation (Non-throwing)

```java
MigoRuntime.Result<GameSession> result = MigoRuntime.getInstance()
    .createSessionSafe(activity, surface, config, "my-game-id");

if (result.isSuccess()) {
    session = result.getValue();
    session.startGame("game.js");  // uses paths.getCodeDir()
} else {
    int errorCode = result.getErrorCode();
    String message = result.getErrorMessage();
    Log.e(TAG, "Failed to create session: " + ErrorCode.getMessage(errorCode));
}
```

### Register Host Handlers (Auth / GameLog / Subpackage)

In the latest API, `GameSession` supports three host callback handlers.
Register them before `startGame()` whenever possible.

```java
session.setAuthHandler(new AuthHandler() {
    @Override
    public void login(int timeoutMs, LoginCallback callback) {
        callback.onFailure("not implemented");
    }

    @Override
    public void checkSession(CheckSessionCallback callback) {
        callback.onFailure("not implemented");
    }
});

session.setGameLogHandler(logJson -> {
    Log.i("GameLog", logJson);
});

session.setSubpackageHandler((request, callback) -> {
    callback.onFailure("download not implemented");
});
```

- `setAuthHandler(AuthHandler)`: backs `migo.login` / `migo.checkSession` / `migo.getUserInfo` / `migo.getPhoneNumber`, including calls forwarded by an external compatibility adapter
- `setGameLogHandler(GameLogHandler)`: receives game-reported logs (JSON string)
- `setSubpackageHandler(SubpackageHandler)`: handles `loadSubpackage` / `preDownloadSubpackage` downloads

## Configuration Options

```java
RuntimeConfig config = new RuntimeConfig.Builder(context)
    // Performance
    .setTargetFps(60)              // 1-240, default: 60
    
    // Debugging
    .setDebugEnabled(true)         // Enable debug features
    .setLogLevel(LogLevel.DEBUG)   // TRACE, DEBUG, INFO, WARN, ERROR, OFF
    
    // Directories
    .setCodeCacheDir(codeCacheDir) // For compiled code
    
    .build();
```

## Error Handling

The SDK uses structured error codes for all operations:

```java
// Error codes
ErrorCode.SUCCESS               //  0: Success
ErrorCode.ERR_INIT_FAILED       // -1000: Initialization failed
ErrorCode.ERR_INVALID_SURFACE   // -1001: Invalid Surface
ErrorCode.ERR_INVALID_CONFIG    // -1002: Invalid configuration
ErrorCode.ERR_NATIVE_LOAD_FAILED// -1003: Native library load failed
ErrorCode.ERR_SESSION_DESTROYED // -2000: Session destroyed
ErrorCode.ERR_CODE_DIR_NOT_FOUND// -2002: Code directory not found
ErrorCode.ERR_ENTRY_NOT_FOUND   // -2003: Entry point not found
ErrorCode.ERR_JS_EXECUTION      // -2004: JavaScript execution error
ErrorCode.ERR_INVALID_ACTIVITY  // -5004: Invalid Activity

// Get human-readable message
String message = ErrorCode.getMessage(code);
```

## API Reference

### MigoRuntime

The main entry point (singleton):

| Method | Description |
|--------|-------------|
| `getInstance()` | Get the singleton instance |
| `createSession(Activity, Surface, RuntimeConfig, String gameId)` | Create a game session (Activity-bound) |
| `createSession(Context, Surface, RuntimeConfig, String gameId)` | Create a game session (without Activity binding) |
| `createSessionSafe(Activity, Surface, RuntimeConfig, String gameId)` | Non-throwing version |
| `getVersion()` | Get SDK version |
| `getNativeVersion()` | Get native engine version |
| `isNativeLoaded()` | Check if native library loaded |
| `isDeviceSupported()` | Check device compatibility |
| `getActiveSessionCount()` | Get active session count |
| `getMinSdkVersion()` | Get minimum supported API level |

### GameSession

Represents an active game session (implements `Closeable`):

| Method | Description |
|--------|-------------|
| `startGame(String entryPoint)` | Start game (from `paths.getCodeDir()`) |
| `startGameSafe(String entryPoint)` | Non-throwing version |
| `pause()` | Pause the game |
| `resume()` | Resume the game |
| `restart()` | Restart the game |
| `updateSurface(Surface)` | Update rendering surface |
| `dispatchTouchEvent(MotionEvent)` | Handle touch input |
| `dispatchMemoryWarning(int)` | Forward memory pressure signal |
| `setListener(GameSessionListener)` | Register unified session listener |
| `setAuthHandler(AuthHandler)` | Register auth handler |
| `setGameLogHandler(GameLogHandler)` | Register game log handler |
| `setSubpackageHandler(SubpackageHandler)` | Register subpackage download handler |
| `close()` / `destroy()` | Release resources |
| `isValid()` | Check if session is valid |
| `isGameStarted()` | Check if game started |

### RuntimeConfig.Builder

Configuration builder:

| Method | Default | Description |
|--------|---------|-------------|
| `setTargetFps(int)` | 60 | Target frame rate (1-240) |
| `setDebugEnabled(boolean)` | false | Debug mode |
| `setLogLevel(LogLevel)` | WARN | Log verbosity |
| `setCodeCacheDir(String)` | cacheDir | Compiled code directory |

## ProGuard

The library includes ProGuard rules. If you need to add custom rules:

```proguard
# Keep public API
-keep public class com.migo.runtime.** { public *; }

# Keep callback interfaces
-keep interface com.migo.runtime.callback.** { *; }
```

## Requirements

- **Minimum SDK**: 26 (Android 8.0 Oreo)
- **Target SDK**: 34 (Android 14)
- **Supported ABIs**: arm64-v8a, x86_64

## License

See the project root for license information.
