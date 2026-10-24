package com.migo.runtime.internal.platform;

import android.content.Context;
import android.content.res.ColorStateList;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Path;
import android.view.GestureDetector;
import android.view.Gravity;
import android.view.MotionEvent;
import android.view.View;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.SeekBar;
import android.widget.TextView;

import org.json.JSONObject;

import java.util.Locale;

/**
 * A video's own controls, over its picture, as content's options ask:
 *
 * <ul>
 *   <li>{@code controls}: a bar along the bottom -- play or pause, the time, a
 *       progress bar ({@code showProgressInControlMode}), the duration, full
 *       screen. A tap shows it; while playing it hides again after
 *       {@link #BAR_SHOWN_MS}.</li>
 *   <li>{@code showProgress}: while the bar is hidden, a thin progress line
 *       along the bottom.</li>
 *   <li>{@code showCenterPlayBtn}: a play button in the middle while it is not
 *       playing.</li>
 *   <li>{@code enablePlayGesture}: a double tap plays or pauses.</li>
 *   <li>{@code enableProgressGesture}: a sideways drag seeks, the whole width
 *       being the whole video, the time shown as it goes.</li>
 * </ul>
 *
 * <p>What the player does through them content hears as it hears its own calls:
 * play, pause, the time. With none of them asked for, the video takes no touch,
 * and a touch on it is the game's.
 *
 * @hide
 */
final class VideoControls extends FrameLayout {

    /** The player the controls drive. Main thread only. */
    interface Player {
        boolean isPlaying();

        /** Seconds; 0 before it knows. */
        double position();

        /** Seconds; 0 before it knows. */
        double duration();

        void togglePlay();

        void seekTo(double seconds);

        boolean isFullScreen();

        void toggleFullScreen();
    }

    /** How long the bar stays after it is shown, while playing. */
    static final long BAR_SHOWN_MS = 3000;
    private static final int BAR_HEIGHT_DP = 40;
    private static final int ICON_DP = 24;
    private static final int CENTER_DP = 56;
    private static final int LINE_DP = 2;
    private static final int TEXT_SP = 12;

    private final Player player;
    private final float density;
    private final LinearLayout bar;
    private final PlayIcon barPlay;
    private final TextView time;
    private final SeekBar seek;
    private final TextView duration;
    private final FullScreenIcon fullScreen;
    private final ProgressLine line;
    private final PlayIcon center;
    private final TextView seeking;
    private final Gestures listener = new Gestures();
    private final GestureDetector gestures;
    /** Hides the bar once the player has been left alone while playing. */
    private final Runnable autoHide = this::hideIfStillPlaying;
    private boolean autoHiding;

    private boolean controls;
    private boolean showProgress;
    private boolean showProgressInBar;
    private boolean showCenter;
    private boolean playGesture;
    private boolean progressGesture;
    private boolean barShown;
    /** While the progress bar or a drag is seeking: where it is going. */
    private double seekTarget = -1;
    private double dragFrom;
    private boolean dragging;

    VideoControls(Context context, Player player) {
        super(context);
        this.player = player;
        this.density = context.getResources().getDisplayMetrics().density;

        line = new ProgressLine(context);
        addView(line, new LayoutParams(LayoutParams.MATCH_PARENT, px(LINE_DP), Gravity.BOTTOM));

        center = new PlayIcon(context, true);
        center.setContentDescription("play");
        center.setOnClickListener(v -> player.togglePlay());
        addView(center, new LayoutParams(px(CENTER_DP), px(CENTER_DP), Gravity.CENTER));

        seeking = text(context);
        seeking.setTextSize(TEXT_SP + 4);
        seeking.setVisibility(GONE);
        addView(seeking, new LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT,
                Gravity.CENTER));

        bar = new LinearLayout(context);
        bar.setOrientation(LinearLayout.HORIZONTAL);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        bar.setBackgroundColor(Color.argb(0x80, 0, 0, 0));
        int pad = px(8);
        bar.setPadding(pad, 0, pad, 0);
        barPlay = new PlayIcon(context, false);
        barPlay.setContentDescription("play or pause");
        barPlay.setOnClickListener(v -> {
            player.togglePlay();
            showBarForAWhile();
        });
        bar.addView(barPlay, new LinearLayout.LayoutParams(px(ICON_DP), px(ICON_DP)));
        time = text(context);
        bar.addView(time, spaced(LinearLayout.LayoutParams.WRAP_CONTENT, 0));
        seek = new SeekBar(context);
        seek.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            @Override
            public void onProgressChanged(SeekBar bar, int progress, boolean fromUser) {
                if (!fromUser) return;
                seekTarget = progress / 1000.0 * player.duration();
                time.setText(clock(seekTarget));
            }

            @Override
            public void onStartTrackingTouch(SeekBar bar) {
                cancelAutoHide();
            }

            @Override
            public void onStopTrackingTouch(SeekBar bar) {
                if (seekTarget >= 0) player.seekTo(seekTarget);
                seekTarget = -1;
                showBarForAWhile();
            }
        });
        seek.setMax(1000);
        // The controls' own white, not the host app's theme accent.
        seek.setProgressTintList(ColorStateList.valueOf(Color.WHITE));
        seek.setThumbTintList(ColorStateList.valueOf(Color.WHITE));
        seek.setProgressBackgroundTintList(ColorStateList.valueOf(Color.GRAY));
        bar.addView(seek, spaced(0, 1));
        duration = text(context);
        bar.addView(duration, spaced(LinearLayout.LayoutParams.WRAP_CONTENT, 0));
        fullScreen = new FullScreenIcon(context);
        fullScreen.setContentDescription("full screen");
        fullScreen.setOnClickListener(v -> {
            player.toggleFullScreen();
            showBarForAWhile();
        });
        bar.addView(fullScreen, new LinearLayout.LayoutParams(px(ICON_DP), px(ICON_DP)));
        addView(bar, new LayoutParams(LayoutParams.MATCH_PARENT, px(BAR_HEIGHT_DP), Gravity.BOTTOM));
        // Hidden until a tap shows it.
        bar.setVisibility(GONE);

        gestures = new GestureDetector(context, listener);
        configure(new JSONObject());
    }

    /** Take content's options, as created or as it last set them. */
    void configure(JSONObject options) {
        controls = options.optBoolean("controls", true);
        showProgress = options.optBoolean("showProgress", true);
        showProgressInBar = options.optBoolean("showProgressInControlMode", true);
        showCenter = options.optBoolean("showCenterPlayBtn", true);
        playGesture = options.optBoolean("enablePlayGesture", false);
        progressGesture = options.optBoolean("enableProgressGesture", true);
        // Listening for double taps holds a single tap back until it is not
        // one; without the play gesture a tap answers at once.
        gestures.setOnDoubleTapListener(playGesture ? listener : null);
        seek.setVisibility(showProgressInBar ? VISIBLE : INVISIBLE);
        if (!controls) setBarShown(false);
        refresh();
    }

    /** Whether any touch on the video is the controls'. */
    boolean takesTouches() {
        return controls || playGesture || progressGesture || (showCenter && !player.isPlaying());
    }

    /** The player changed: playing, paused, a new time, a new size. */
    void refresh() {
        boolean playing = player.isPlaying();
        double total = player.duration();
        double now = seekTarget >= 0 ? seekTarget : player.position();
        center.setVisibility(showCenter && !playing && !dragging ? VISIBLE : GONE);
        barPlay.setPlaying(playing);
        fullScreen.setFullScreen(player.isFullScreen());
        time.setText(clock(now));
        duration.setText(clock(total));
        float fraction = total > 0 ? (float) Math.min(1, now / total) : 0f;
        if (seekTarget < 0) seek.setProgress(Math.round(fraction * 1000));
        line.setFraction(fraction);
        line.setVisibility(showProgress && !barShown ? VISIBLE : GONE);
        if (barShown && playing && !autoHiding && seekTarget < 0) {
            autoHiding = true;
            postDelayed(autoHide, BAR_SHOWN_MS);
        }
    }

    private void hideIfStillPlaying() {
        autoHiding = false;
        if (player.isPlaying()) setBarShown(false);
    }

    private void cancelAutoHide() {
        removeCallbacks(autoHide);
        autoHiding = false;
    }

    private void showBarForAWhile() {
        cancelAutoHide();
        setBarShown(controls);
    }

    private void toggleBar() {
        if (barShown) {
            setBarShown(false);
        } else {
            showBarForAWhile();
        }
    }

    private void setBarShown(boolean shown) {
        barShown = shown && controls;
        bar.setVisibility(barShown ? VISIBLE : GONE);
        if (!barShown) cancelAutoHide();
        refresh();
    }

    boolean isBarShown() {
        return barShown;
    }

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        if (!takesTouches()) return false;
        boolean handled = gestures.onTouchEvent(event);
        int action = event.getActionMasked();
        if (dragging && (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL)) {
            dragging = false;
            seeking.setVisibility(GONE);
            if (action == MotionEvent.ACTION_UP && seekTarget >= 0) player.seekTo(seekTarget);
            seekTarget = -1;
            refresh();
        }
        return handled || action == MotionEvent.ACTION_DOWN || dragging;
    }

    private final class Gestures extends GestureDetector.SimpleOnGestureListener {
        @Override
        public boolean onDown(MotionEvent e) {
            dragFrom = player.position();
            return true;
        }

        @Override
        public boolean onSingleTapUp(MotionEvent e) {
            // With the play gesture, a single tap is the one confirmed below.
            if (playGesture || !controls) return false;
            toggleBar();
            return true;
        }

        @Override
        public boolean onSingleTapConfirmed(MotionEvent e) {
            if (!controls) return false;
            toggleBar();
            return true;
        }

        @Override
        public boolean onDoubleTap(MotionEvent e) {
            if (!playGesture) return false;
            player.togglePlay();
            return true;
        }

        @Override
        public boolean onScroll(MotionEvent down, MotionEvent move, float dx, float dy) {
            double total = player.duration();
            if (!progressGesture || total <= 0 || getWidth() <= 0 || down == null) return false;
            float across = move.getX() - down.getX();
            if (!dragging && Math.abs(across) <= Math.abs(move.getY() - down.getY())) return false;
            dragging = true;
            seekTarget = Math.max(0, Math.min(total, dragFrom + across / getWidth() * total));
            seeking.setText(clock(seekTarget) + " / " + clock(total));
            seeking.setVisibility(VISIBLE);
            refresh();
            return true;
        }
    }

    /** {@code seconds} as the bar shows it: m:ss, or h:mm:ss from an hour. */
    static String clock(double seconds) {
        long whole = Math.max(0, Math.round(Math.floor(seconds)));
        long hours = whole / 3600;
        long minutes = whole / 60 % 60;
        long secs = whole % 60;
        return hours > 0
                ? String.format(Locale.ROOT, "%d:%02d:%02d", hours, minutes, secs)
                : String.format(Locale.ROOT, "%d:%02d", minutes, secs);
    }

    private TextView text(Context context) {
        TextView view = new TextView(context);
        view.setTextColor(Color.WHITE);
        view.setTextSize(TEXT_SP);
        view.setSingleLine(true);
        return view;
    }

    private LinearLayout.LayoutParams spaced(int width, float weight) {
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
                width, LinearLayout.LayoutParams.WRAP_CONTENT, weight);
        params.leftMargin = px(8);
        params.rightMargin = px(8);
        return params;
    }

    private int px(int dp) {
        return Math.round(dp * density);
    }

    /** A play triangle, or two pause bars while playing; ringed in the middle. */
    static final class PlayIcon extends View {
        private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Path path = new Path();
        private final boolean ringed;
        private boolean playing;

        PlayIcon(Context context, boolean ringed) {
            super(context);
            this.ringed = ringed;
            paint.setColor(Color.WHITE);
        }

        void setPlaying(boolean playing) {
            if (this.playing == playing) return;
            this.playing = playing;
            invalidate();
        }

        boolean isPlaying() {
            return playing;
        }

        @Override
        protected void onDraw(Canvas canvas) {
            float w = getWidth();
            float h = getHeight();
            if (ringed) {
                paint.setStyle(Paint.Style.FILL);
                paint.setColor(Color.argb(0x80, 0, 0, 0));
                canvas.drawCircle(w / 2, h / 2, Math.min(w, h) / 2, paint);
                paint.setColor(Color.WHITE);
            }
            float inset = ringed ? 0.3f : 0.2f;
            float left = w * inset;
            float right = w * (1 - inset);
            float top = h * inset;
            float bottom = h * (1 - inset);
            paint.setStyle(Paint.Style.FILL);
            if (playing) {
                float bar = (right - left) / 3;
                canvas.drawRect(left, top, left + bar, bottom, paint);
                canvas.drawRect(right - bar, top, right, bottom, paint);
            } else {
                path.reset();
                path.moveTo(left + (right - left) * 0.1f, top);
                path.lineTo(right, (top + bottom) / 2);
                path.lineTo(left + (right - left) * 0.1f, bottom);
                path.close();
                canvas.drawPath(path, paint);
            }
        }
    }

    /** Four corners: out to full screen, in to leave it. */
    static final class FullScreenIcon extends View {
        private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
        private boolean fullScreen;

        FullScreenIcon(Context context) {
            super(context);
            paint.setColor(Color.WHITE);
            paint.setStyle(Paint.Style.STROKE);
            paint.setStrokeWidth(2 * context.getResources().getDisplayMetrics().density);
        }

        void setFullScreen(boolean fullScreen) {
            if (this.fullScreen == fullScreen) return;
            this.fullScreen = fullScreen;
            invalidate();
        }

        @Override
        protected void onDraw(Canvas canvas) {
            float w = getWidth();
            float h = getHeight();
            float a = w * 0.2f;
            float b = w * 0.8f;
            float c = h * 0.2f;
            float d = h * 0.8f;
            float arm = w * 0.2f;
            // Each corner's two strokes point out of the box, or into it.
            float sign = fullScreen ? -1 : 1;
            float[][] corners = {{a, c, 1, 1}, {b, c, -1, 1}, {a, d, 1, -1}, {b, d, -1, -1}};
            for (float[] corner : corners) {
                float x = fullScreen ? corner[0] + corner[2] * arm : corner[0];
                float y = fullScreen ? corner[1] + corner[3] * arm : corner[1];
                canvas.drawLine(x, y, x + sign * corner[2] * arm, y, paint);
                canvas.drawLine(x, y, x, y + sign * corner[3] * arm, paint);
            }
        }
    }

    /** The thin line of how far it has played. */
    static final class ProgressLine extends View {
        private final Paint paint = new Paint();
        private float fraction;

        ProgressLine(Context context) {
            super(context);
            paint.setColor(Color.WHITE);
        }

        void setFraction(float fraction) {
            if (this.fraction == fraction) return;
            this.fraction = fraction;
            invalidate();
        }

        @Override
        protected void onDraw(Canvas canvas) {
            canvas.drawRect(0, 0, getWidth() * fraction, getHeight(), paint);
        }
    }
}
