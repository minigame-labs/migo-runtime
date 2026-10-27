package com.migo.runtime.internal.platform;

import android.app.Activity;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Color;
import android.graphics.Matrix;
import android.graphics.SurfaceTexture;
import android.media.MediaPlayer;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.view.Surface;
import android.view.TextureView;
import android.view.View;
import android.widget.FrameLayout;
import android.widget.ImageView;

import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.RuntimeGenerationBoundary;
import com.migo.runtime.internal.RuntimeScoped;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.InputStream;
import java.lang.ref.WeakReference;
import java.net.HttpURLConnection;
import java.net.URL;
import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ConcurrentMap;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * A session's video players: each a MediaPlayer drawing into a TextureView over
 * the game, where content put it, in the game's logical pixels.
 *
 * <p>Every request is the one content made, {@code {"videoId", ...}}. A call --
 * play, pause, stop, seek, the full-screen pair -- is answered through
 * {@link NativeMethods#onVideoResult} once it has happened, as the reference's
 * promises settle; what a player does as it plays goes to
 * {@link NativeMethods#onVideoEvent} with its {@code videoId}. Everything a
 * player does runs on the main thread.
 *
 * @hide
 */
public class VideoManager implements RuntimeScoped {
    private static final String TAG = "VideoManager";

    /** The calls and events by number, as the engine numbers them
     *  ({@code shared::services::video_method}, {@code video_event}). */
    public static final int METHOD_PLAY = 3;
    public static final int METHOD_PAUSE = 4;
    public static final int METHOD_STOP = 5;
    public static final int METHOD_SEEK = 6;
    public static final int METHOD_REQUEST_FULL_SCREEN = 7;
    public static final int METHOD_EXIT_FULL_SCREEN = 8;
    public static final int EVENT_PLAY = 0;
    public static final int EVENT_PAUSE = 1;
    public static final int EVENT_ENDED = 2;
    public static final int EVENT_TIME_UPDATE = 3;
    public static final int EVENT_ERROR = 4;
    public static final int EVENT_WAITING = 5;
    public static final int EVENT_PROGRESS = 6;

    /** How often a playing video reports its position. */
    private static final long TIME_UPDATE_MS = 250;
    /** The largest poster fetched from the network. */
    private static final int MAX_POSTER_BYTES = 8 << 20;

    private final int sessionId;
    private final WeakReference<Activity> activityRef;
    private final View gameView;
    private final Handler mainHandler = new Handler(Looper.getMainLooper());
    private final ConcurrentMap<Integer, VideoPlayer> players = new ConcurrentHashMap<>();
    private final AtomicBoolean lifecycleSuspendedTarget;
    private final AtomicBoolean destroyed = new AtomicBoolean(false);
    /** Main thread only. */
    private boolean lifecycleSuspended;
    /** Network posters, fetched off the main thread; made when first needed. */
    private ExecutorService posterLoader;

    /** The runtime this manager belongs to, which tears it down when it goes.
     *  What it reports names its player rather than the runtime. */
    private final RuntimeGenerationBoundary.Token token;

    @Override
    public RuntimeGenerationBoundary.Token runtimeToken() {
        return token;
    }

    /**
     * @param gameView the game's view, which a player's rectangle is from
     */
    public VideoManager(int sessionId, Activity activity, View gameView, boolean lifecycleSuspended) {
        this.sessionId = sessionId;
        this.token = RuntimeGenerationBoundary.acquire(sessionId);
        this.activityRef = new WeakReference<>(activity);
        this.gameView = gameView;
        this.lifecycleSuspendedTarget = new AtomicBoolean(lifecycleSuspended);
        this.lifecycleSuspended = lifecycleSuspended;
    }

    /**
     * The video id a request carries.
     *
     * <p>The runtime allocates it and routes every event through it, so Android
     * honours it rather than numbering its own. {@code optInt} is deliberately
     * not used: it truncates, so {@code 1.5} reads back as video 1 -- an id that
     * names a different, live player.
     */
    static int requestedVideoId(JSONObject request) {
        Object raw = request == null ? null : request.opt("videoId");
        long id;
        if (raw instanceof Integer) {
            id = (Integer) raw;
        } else if (raw instanceof Long) {
            id = (Long) raw;
        } else {
            throw new IllegalArgumentException("videoId is missing or not an integer");
        }
        if (id <= 0 || id > Integer.MAX_VALUE) {
            throw new IllegalArgumentException("videoId " + id + " is outside 1..2147483647");
        }
        return (int) id;
    }

    /**
     * Registers {@code player} under {@code videoId}, refusing an id already live.
     * {@code putIfAbsent} decides and inserts in one step, so two creates racing
     * for one id cannot both be told they have it.
     */
    static <T> void claimVideoId(ConcurrentMap<Integer, T> live, int videoId, T player) {
        if (live.putIfAbsent(videoId, player) != null) {
            throw new IllegalStateException("videoId " + videoId + " is already live");
        }
    }

    // ==================== Requests ====================

    /** Make a player and show it: a command. A player that cannot be made says
     *  so through its ERROR event. */
    public void create(String requestJson) {
        JSONObject request;
        int videoId;
        try {
            request = new JSONObject(requestJson);
            videoId = requestedVideoId(request);
        } catch (JSONException | IllegalArgumentException e) {
            Log.w(TAG, "createVideo refused: " + e.getMessage());
            return;
        }
        Activity activity = activityRef.get();
        if (destroyed.get() || activity == null || gameView == null) {
            fireEvent(videoId, EVENT_ERROR, error("createVideo:fail no game view"));
            return;
        }
        VideoPlayer player = new VideoPlayer(videoId, request);
        try {
            claimVideoId(players, videoId, player);
        } catch (IllegalStateException e) {
            fireEvent(videoId, EVENT_ERROR, error("createVideo:fail " + e.getMessage()));
            return;
        }
        if (destroyed.get() && players.remove(videoId, player)) return;
        mainHandler.post(() -> player.show(activity));
    }

    /** Take a player away: a command. Its calls still unanswered fail. */
    public void destroy(String requestJson) {
        VideoPlayer player = players.remove(videoIdOf(requestJson));
        if (player != null) mainHandler.post(() -> player.release("video destroyed"));
    }

    /** What content changed on a player: a command. */
    public void set(String requestJson) {
        JSONObject changed;
        try {
            changed = new JSONObject(requestJson);
        } catch (JSONException e) {
            return;
        }
        VideoPlayer player = players.get(videoIdOf(requestJson));
        if (player != null) mainHandler.post(() -> player.set(changed));
    }

    public void play(String requestJson) {
        call(requestJson, METHOD_PLAY, VideoPlayer::play);
    }

    public void pause(String requestJson) {
        call(requestJson, METHOD_PAUSE, VideoPlayer::pause);
    }

    public void stop(String requestJson) {
        call(requestJson, METHOD_STOP, VideoPlayer::stop);
    }

    public void seek(String requestJson) {
        call(requestJson, METHOD_SEEK, VideoPlayer::seek);
    }

    public void requestFullScreen(String requestJson) {
        call(requestJson, METHOD_REQUEST_FULL_SCREEN, VideoPlayer::requestFullScreen);
    }

    public void exitFullScreen(String requestJson) {
        call(requestJson, METHOD_EXIT_FULL_SCREEN, VideoPlayer::exitFullScreen);
    }

    /** What a call does on its player, on the main thread. */
    private interface CallOnPlayer {
        void run(VideoPlayer player, int requestId, JSONObject request);
    }

    private void call(String requestJson, int method, CallOnPlayer action) {
        JSONObject request;
        try {
            request = new JSONObject(requestJson);
        } catch (JSONException e) {
            return;
        }
        int requestId = request.optInt("requestId", 0);
        VideoPlayer player = players.get(videoIdOf(requestJson));
        if (player == null) {
            answer(method, requestId, "video not found");
            return;
        }
        mainHandler.post(() -> {
            if (player.released) {
                answer(method, requestId, "video destroyed");
            } else {
                action.run(player, requestId, request);
            }
        });
    }

    /** Every player released with the session. */
    public void destroy() {
        if (!destroyed.compareAndSet(false, true)) return;
        mainHandler.post(() -> {
            for (VideoPlayer player : players.values()) player.release("video destroyed");
            players.clear();
            if (posterLoader != null) posterLoader.shutdownNow();
        });
    }

    // ==================== Lifecycle ====================

    public void setLifecycleSuspended(boolean suspended) {
        if (destroyed.get()) return;
        lifecycleSuspendedTarget.set(suspended);
        mainHandler.post(this::applyLifecycleTarget);
    }

    public void suspendForLifecycle() {
        setLifecycleSuspended(true);
    }

    public void resumeForLifecycle() {
        setLifecycleSuspended(false);
    }

    private void applyLifecycleTarget() {
        if (destroyed.get()) return;
        boolean suspended = lifecycleSuspendedTarget.get();
        if (suspended == lifecycleSuspended) return;
        lifecycleSuspended = suspended;
        for (VideoPlayer player : players.values()) {
            if (suspended) {
                player.suspendForLifecycle();
            } else {
                player.resumeForLifecycle();
            }
        }
    }

    // ==================== Answers and events ====================

    private static int videoIdOf(String requestJson) {
        try {
            return requestedVideoId(new JSONObject(requestJson));
        } catch (JSONException | IllegalArgumentException e) {
            return 0;
        }
    }

    private static JSONObject error(String errMsg) {
        JSONObject payload = new JSONObject();
        try {
            payload.put("errMsg", errMsg);
        } catch (JSONException ignored) {
            // A string always fits.
        }
        return payload;
    }

    /** Answer call {@code requestId}: done, or failed with {@code reason}. */
    private void answer(int method, int requestId, String reason) {
        JSONObject result = new JSONObject();
        try {
            result.put("requestId", requestId);
            if (reason != null) result.put("error", reason);
        } catch (JSONException ignored) {
            // An int and a string always fit.
        }
        NativeMethods.onVideoResult(sessionId, method, result.toString());
    }

    private void fireEvent(int videoId, int event, JSONObject payload) {
        try {
            payload.put("videoId", videoId);
        } catch (JSONException ignored) {
            // An int always fits.
        }
        NativeMethods.onVideoEvent(sessionId, event, payload.toString());
    }

    /**
     * The scale, about the view's centre, that shows a picture of
     * {@code videoWidth} x {@code videoHeight} in a view of {@code viewWidth} x
     * {@code viewHeight} as {@code objectFit} asks -- "fill" stretches it, "cover"
     * fills the view and crops, "contain" fits it whole -- given that the view
     * stretches the picture to its own size first.
     *
     * @return {scale x, scale y}
     */
    static float[] fitScale(String objectFit, int viewWidth, int viewHeight,
                            int videoWidth, int videoHeight) {
        if ("fill".equals(objectFit) || viewWidth <= 0 || viewHeight <= 0
                || videoWidth <= 0 || videoHeight <= 0) {
            return new float[] {1f, 1f};
        }
        float byWidth = (float) viewWidth / videoWidth;
        float byHeight = (float) viewHeight / videoHeight;
        float scale = "cover".equals(objectFit)
                ? Math.max(byWidth, byHeight)
                : Math.min(byWidth, byHeight);
        return new float[] {
                scale * videoWidth / viewWidth,
                scale * videoHeight / viewHeight,
        };
    }

    // ==================== A player ====================

    /** One player: its options, its MediaPlayer and its view. Main thread only,
     *  but for the construction. */
    private final class VideoPlayer implements TextureView.SurfaceTextureListener,
            MediaPlayer.OnPreparedListener, MediaPlayer.OnCompletionListener,
            MediaPlayer.OnErrorListener, MediaPlayer.OnBufferingUpdateListener,
            MediaPlayer.OnInfoListener, MediaPlayer.OnSeekCompleteListener,
            MediaPlayer.OnVideoSizeChangedListener, VideoControls.Player {

        final int videoId;
        /** The reference's options as content last set them. */
        final JSONObject options;
        boolean released;

        FrameLayout container;
        TextureView textureView;
        ImageView posterView;
        /** The player's own controls, as content's options ask. */
        VideoControls controls;
        GameViewOverlay overlay;
        Surface surface;
        MediaPlayer mediaPlayer;
        boolean prepared;
        boolean playing;
        /** Content wants it playing: play() or autoplay, until pause or stop. */
        boolean playWanted;
        boolean fullScreen;
        /** The full screen's turn: 0, 90 (counterclockwise) or -90. */
        int fullScreenDirection;
        int videoWidth;
        int videoHeight;
        final Matrix transform = new Matrix();

        /** Calls waiting to be answered: plays until it plays, seeks until it
         *  is there. */
        final List<Integer> pendingPlays = new ArrayList<>();
        final List<Integer> pendingSeeks = new ArrayList<>();
        /** Where to go once prepared: initialTime, or a seek asked before. */
        double startAt;
        boolean resumeAfterLifecyclePause;
        final Runnable timeUpdate = this::reportTime;

        VideoPlayer(int videoId, JSONObject options) {
            this.videoId = videoId;
            this.options = options;
            this.startAt = options.optDouble("initialTime", 0);
        }

        // ---- the view ----

        void show(Activity activity) {
            if (released) return;
            container = new FrameLayout(activity);
            container.setBackgroundColor(backgroundColor());
            textureView = new TextureView(activity);
            textureView.setSurfaceTextureListener(this);
            textureView.addOnLayoutChangeListener((v, l, t, r, b, ol, ot, or, ob) -> fit());
            container.addView(textureView, new FrameLayout.LayoutParams(
                    FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));
            posterView = new ImageView(activity);
            posterView.setScaleType(ImageView.ScaleType.FIT_CENTER);
            container.addView(posterView, new FrameLayout.LayoutParams(
                    FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));
            controls = new VideoControls(activity, this);
            controls.configure(options);
            container.addView(controls, new FrameLayout.LayoutParams(
                    FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));
            overlay = new GameViewOverlay(activity, gameView, container);
            place();
            loadPoster();
            openSource();
            playWanted = options.optBoolean("autoplay", false);
        }

        int backgroundColor() {
            try {
                return Color.parseColor(options.optString("backgroundColor", "#000000"));
            } catch (IllegalArgumentException e) {
                return Color.BLACK;
            }
        }

        /** Where content put the player, or the whole of the game when full screen. */
        void place() {
            if (overlay == null) return;
            if (fullScreen) {
                View root = gameView.getRootView().findViewById(android.R.id.content);
                float density = DisplayCompat.getDensity(gameView.getContext());
                int direction = fullScreenDirection;
                float width = root.getWidth() / density;
                float height = root.getHeight() / density;
                boolean turned = direction != 0;
                // Turned a quarter, the view is laid out with the screen's sides
                // swapped and rotated about its centre into place.
                float w = turned ? height : width;
                float h = turned ? width : height;
                overlay.place((width - w) / 2, (height - h) / 2, w, h);
                container.setRotation(direction);
            } else {
                overlay.place((float) options.optDouble("x", 0), (float) options.optDouble("y", 0),
                        (float) options.optDouble("width", 300), (float) options.optDouble("height", 150));
                container.setRotation(0);
            }
        }

        /** The picture as objectFit asks, in whatever size the view now has. */
        void fit() {
            if (textureView == null) return;
            float[] scale = fitScale(options.optString("objectFit", "contain"),
                    textureView.getWidth(), textureView.getHeight(), videoWidth, videoHeight);
            transform.setScale(scale[0], scale[1],
                    textureView.getWidth() / 2f, textureView.getHeight() / 2f);
            textureView.setTransform(transform);
        }

        void loadPoster() {
            String poster = options.optString("poster", "");
            posterView.setImageDrawable(null);
            if (poster.isEmpty()) return;
            if (!poster.startsWith("http://") && !poster.startsWith("https://")) {
                showPoster(poster, BitmapFactory.decodeFile(poster));
                return;
            }
            if (posterLoader == null) posterLoader = Executors.newSingleThreadExecutor();
            posterLoader.execute(() -> {
                Bitmap bitmap = fetchPoster(poster);
                mainHandler.post(() -> showPoster(poster, bitmap));
            });
        }

        /** The poster, if it is still the one content wants and nothing has played. */
        void showPoster(String poster, Bitmap bitmap) {
            if (released || bitmap == null || !poster.equals(options.optString("poster", ""))) return;
            if (!playing) posterView.setImageBitmap(bitmap);
        }

        // ---- the source ----

        void openSource() {
            releaseMediaPlayer();
            String src = options.optString("src", "");
            if (src.isEmpty()) return;
            try {
                mediaPlayer = new MediaPlayer();
                mediaPlayer.setDataSource(src);
                mediaPlayer.setOnPreparedListener(this);
                mediaPlayer.setOnCompletionListener(this);
                mediaPlayer.setOnErrorListener(this);
                mediaPlayer.setOnBufferingUpdateListener(this);
                mediaPlayer.setOnInfoListener(this);
                mediaPlayer.setOnSeekCompleteListener(this);
                mediaPlayer.setOnVideoSizeChangedListener(this);
                if (surface != null) mediaPlayer.setSurface(surface);
                applyAudio();
                mediaPlayer.prepareAsync();
            } catch (Exception e) {
                releaseMediaPlayer();
                failPending(String.valueOf(e.getMessage()));
                fireEvent(videoId, EVENT_ERROR, error("video:fail " + e.getMessage()));
            }
        }

        void applyAudio() {
            if (mediaPlayer == null) return;
            float volume = options.optBoolean("muted", false) ? 0f : 1f;
            mediaPlayer.setVolume(volume, volume);
            mediaPlayer.setLooping(options.optBoolean("loop", false));
        }

        void applyRate() {
            if (mediaPlayer == null || !prepared) return;
            float rate = (float) options.optDouble("playbackRate", 1.0);
            try {
                mediaPlayer.setPlaybackParams(mediaPlayer.getPlaybackParams().setSpeed(rate));
            } catch (IllegalArgumentException | IllegalStateException e) {
                Log.w(TAG, "playbackRate " + rate + " not taken: " + e.getMessage());
            }
            // Setting a speed starts a paused player: put it back.
            if (!playing && mediaPlayer.isPlaying()) mediaPlayer.pause();
        }

        void releaseMediaPlayer() {
            stopTimeUpdates();
            if (mediaPlayer != null) {
                mediaPlayer.release();
                mediaPlayer = null;
            }
            prepared = false;
            playing = false;
            videoWidth = 0;
            videoHeight = 0;
        }

        // ---- calls ----

        void play(int requestId, JSONObject request) {
            pendingPlays.add(requestId);
            if (mediaPlayer == null) {
                failPlays(options.optString("src", "").isEmpty() ? "no src" : "the src cannot be played");
                return;
            }
            playWanted = true;
            if (playing) {
                failPlays(null);
            } else if (prepared && !lifecycleSuspended) {
                start();
            }
        }

        void start() {
            mediaPlayer.start();
            playing = true;
            posterView.setImageDrawable(null);
            startTimeUpdates();
            for (Integer requestId : pendingPlays) answer(METHOD_PLAY, requestId, null);
            pendingPlays.clear();
            fireEvent(videoId, EVENT_PLAY, new JSONObject());
            refreshControls();
        }

        void pause(int requestId, JSONObject request) {
            // A play waiting for the player never plays: it is overtaken.
            playWanted = false;
            failPlays("paused before it played");
            pauseNow();
            answer(METHOD_PAUSE, requestId, null);
        }

        /** Pause, as content's call or the player's own control does. */
        void pauseNow() {
            if (playing) {
                mediaPlayer.pause();
                playing = false;
                stopTimeUpdates();
                fireEvent(videoId, EVENT_PAUSE, new JSONObject());
            }
            resumeAfterLifecyclePause = false;
            refreshControls();
        }

        void refreshControls() {
            if (controls != null) controls.refresh();
        }

        // ---- the player's own controls ----

        @Override
        public boolean isPlaying() {
            return playing;
        }

        @Override
        public double position() {
            return prepared ? mediaPlayer.getCurrentPosition() / 1000.0 : 0;
        }

        @Override
        public double duration() {
            return prepared ? Math.max(0, mediaPlayer.getDuration()) / 1000.0 : 0;
        }

        @Override
        public void togglePlay() {
            if (released || mediaPlayer == null) return;
            if (playing) {
                playWanted = false;
                pauseNow();
            } else {
                playWanted = true;
                if (prepared && !lifecycleSuspended) start();
            }
        }

        @Override
        public void seekTo(double seconds) {
            if (released || !prepared) return;
            mediaPlayer.seekTo((long) (seconds * 1000), MediaPlayer.SEEK_CLOSEST);
            refreshControls();
        }

        @Override
        public boolean isFullScreen() {
            return fullScreen;
        }

        @Override
        public void toggleFullScreen() {
            if (released) return;
            fullScreen = !fullScreen;
            // A landscape picture turns the screen a quarter, as content's
            // requestFullScreen with direction 90 does.
            fullScreenDirection = fullScreen && videoWidth > videoHeight ? 90 : 0;
            place();
            refreshControls();
        }

        void stop(int requestId, JSONObject request) {
            playWanted = false;
            failPlays("stopped before it played");
            if (playing) {
                mediaPlayer.pause();
                playing = false;
                stopTimeUpdates();
            }
            resumeAfterLifecyclePause = false;
            if (prepared) {
                mediaPlayer.seekTo(0);
            } else {
                startAt = 0;
            }
            refreshControls();
            answer(METHOD_STOP, requestId, null);
        }

        void seek(int requestId, JSONObject request) {
            double time = request.optDouble("time", 0);
            pendingSeeks.add(requestId);
            if (prepared) {
                mediaPlayer.seekTo((long) (time * 1000), MediaPlayer.SEEK_CLOSEST);
            } else {
                startAt = time;
            }
        }

        void requestFullScreen(int requestId, JSONObject request) {
            fullScreenDirection = request.optInt("direction", 0);
            fullScreen = true;
            place();
            refreshControls();
            // Answered once laid out at its new size.
            container.post(() -> answer(METHOD_REQUEST_FULL_SCREEN, requestId, null));
        }

        void exitFullScreen(int requestId, JSONObject request) {
            fullScreen = false;
            fullScreenDirection = 0;
            place();
            refreshControls();
            container.post(() -> answer(METHOD_EXIT_FULL_SCREEN, requestId, null));
        }

        // ---- what content changed ----

        void set(JSONObject changed) {
            if (released) return;
            boolean source = false;
            boolean poster = false;
            for (Iterator<String> keys = changed.keys(); keys.hasNext(); ) {
                String key = keys.next();
                if ("videoId".equals(key)) continue;
                try {
                    options.put(key, changed.get(key));
                } catch (JSONException ignored) {
                    // A value read from JSON always fits back.
                }
                source |= "src".equals(key);
                poster |= "poster".equals(key);
            }
            if (changed.has("x") || changed.has("y") || changed.has("width")
                    || changed.has("height")) {
                place();
            }
            if (changed.has("objectFit")) fit();
            if (changed.has("backgroundColor")) container.setBackgroundColor(backgroundColor());
            if (changed.has("muted") || changed.has("loop")) applyAudio();
            if (changed.has("controls") || changed.has("showProgress")
                    || changed.has("showProgressInControlMode") || changed.has("showCenterPlayBtn")
                    || changed.has("enablePlayGesture") || changed.has("enableProgressGesture")) {
                controls.configure(options);
            }
            if (changed.has("playbackRate")) applyRate();
            if (poster) loadPoster();
            if (source) {
                playWanted = false;
                failPending("src changed");
                startAt = options.optDouble("initialTime", 0);
                openSource();
            }
        }

        // ---- teardown and lifecycle ----

        void release(String reason) {
            if (released) return;
            released = true;
            failPending(reason);
            releaseMediaPlayer();
            if (surface != null) {
                surface.release();
                surface = null;
            }
            if (overlay != null) overlay.remove();
        }

        /** Answer every play waiting: done when {@code reason} is null, failed
         *  with it otherwise. */
        void failPlays(String reason) {
            for (Integer requestId : pendingPlays) answer(METHOD_PLAY, requestId, reason);
            pendingPlays.clear();
        }

        void failPending(String reason) {
            failPlays(reason);
            for (Integer requestId : pendingSeeks) answer(METHOD_SEEK, requestId, reason);
            pendingSeeks.clear();
        }

        void suspendForLifecycle() {
            if (!playing) return;
            mediaPlayer.pause();
            playing = false;
            stopTimeUpdates();
            resumeAfterLifecyclePause = true;
            refreshControls();
        }

        void resumeForLifecycle() {
            if (released) return;
            if (resumeAfterLifecyclePause || playWanted) {
                resumeAfterLifecyclePause = false;
                if (prepared && !playing) start();
            }
        }

        // ---- reports ----

        void startTimeUpdates() {
            mainHandler.removeCallbacks(timeUpdate);
            mainHandler.postDelayed(timeUpdate, TIME_UPDATE_MS);
        }

        void stopTimeUpdates() {
            mainHandler.removeCallbacks(timeUpdate);
        }

        void reportTime() {
            if (released || !playing || mediaPlayer == null) return;
            JSONObject payload = new JSONObject();
            try {
                payload.put("position", mediaPlayer.getCurrentPosition() / 1000.0);
                payload.put("duration", mediaPlayer.getDuration() / 1000.0);
            } catch (JSONException ignored) {
                // Numbers always fit.
            }
            fireEvent(videoId, EVENT_TIME_UPDATE, payload);
            refreshControls();
            mainHandler.postDelayed(timeUpdate, TIME_UPDATE_MS);
        }

        // ---- TextureView ----

        @Override
        public void onSurfaceTextureAvailable(SurfaceTexture texture, int width, int height) {
            if (released) return;
            surface = new Surface(texture);
            if (mediaPlayer != null) mediaPlayer.setSurface(surface);
            fit();
        }

        @Override
        public void onSurfaceTextureSizeChanged(SurfaceTexture texture, int width, int height) {
            fit();
        }

        @Override
        public boolean onSurfaceTextureDestroyed(SurfaceTexture texture) {
            if (mediaPlayer != null) mediaPlayer.setSurface(null);
            if (surface != null) {
                surface.release();
                surface = null;
            }
            return true;
        }

        @Override
        public void onSurfaceTextureUpdated(SurfaceTexture texture) {}

        // ---- MediaPlayer ----

        @Override
        public void onPrepared(MediaPlayer player) {
            if (released || player != mediaPlayer) return;
            prepared = true;
            applyRate();
            if (startAt > 0) {
                mediaPlayer.seekTo((long) (startAt * 1000), MediaPlayer.SEEK_CLOSEST);
                startAt = 0;
            } else {
                for (Integer requestId : pendingSeeks) answer(METHOD_SEEK, requestId, null);
                pendingSeeks.clear();
            }
            if (playWanted && !lifecycleSuspended) start();
            refreshControls();
        }

        @Override
        public void onSeekComplete(MediaPlayer player) {
            if (released || player != mediaPlayer) return;
            for (Integer requestId : pendingSeeks) answer(METHOD_SEEK, requestId, null);
            pendingSeeks.clear();
            refreshControls();
        }

        @Override
        public void onCompletion(MediaPlayer player) {
            if (released || player != mediaPlayer) return;
            playing = false;
            playWanted = false;
            stopTimeUpdates();
            fireEvent(videoId, EVENT_ENDED, new JSONObject());
            refreshControls();
        }

        @Override
        public boolean onError(MediaPlayer player, int what, int extra) {
            if (released || player != mediaPlayer) return true;
            String reason = "MediaPlayer error " + what + "/" + extra;
            failPending(reason);
            playing = false;
            playWanted = false;
            stopTimeUpdates();
            fireEvent(videoId, EVENT_ERROR, error("video:fail " + reason));
            refreshControls();
            return true;
        }

        @Override
        public boolean onInfo(MediaPlayer player, int what, int extra) {
            if (released || player != mediaPlayer) return false;
            if (what == MediaPlayer.MEDIA_INFO_BUFFERING_START) {
                fireEvent(videoId, EVENT_WAITING, new JSONObject());
            } else if (what == MediaPlayer.MEDIA_INFO_VIDEO_RENDERING_START) {
                posterView.setImageDrawable(null);
            }
            return false;
        }

        @Override
        public void onBufferingUpdate(MediaPlayer player, int percent) {
            if (released || player != mediaPlayer || !prepared) return;
            JSONObject payload = new JSONObject();
            try {
                payload.put("buffered", percent);
                payload.put("duration", mediaPlayer.getDuration() / 1000.0);
            } catch (JSONException ignored) {
                // Numbers always fit.
            }
            fireEvent(videoId, EVENT_PROGRESS, payload);
        }

        @Override
        public void onVideoSizeChanged(MediaPlayer player, int width, int height) {
            if (released || player != mediaPlayer) return;
            videoWidth = width;
            videoHeight = height;
            fit();
        }
    }

    /** A network poster, at most {@link #MAX_POSTER_BYTES}, or null. */
    private static Bitmap fetchPoster(String url) {
        HttpURLConnection connection = null;
        try {
            connection = (HttpURLConnection) new URL(url).openConnection();
            connection.setConnectTimeout(10_000);
            connection.setReadTimeout(10_000);
            if (connection.getContentLengthLong() > MAX_POSTER_BYTES) return null;
            try (InputStream in = connection.getInputStream()) {
                byte[] bytes = new byte[64 * 1024];
                java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream();
                int read;
                while ((read = in.read(bytes)) > 0) {
                    out.write(bytes, 0, read);
                    if (out.size() > MAX_POSTER_BYTES) return null;
                }
                byte[] data = out.toByteArray();
                return BitmapFactory.decodeByteArray(data, 0, data.length);
            }
        } catch (Exception e) {
            Log.w(TAG, "poster not fetched: " + e.getMessage());
            return null;
        } finally {
            if (connection != null) connection.disconnect();
        }
    }
}
