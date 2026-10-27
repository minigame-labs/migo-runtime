package com.migo.runtime.internal.platform;

import android.app.Activity;
import android.graphics.Rect;
import android.os.Handler;
import android.os.Looper;
import android.text.Editable;
import android.text.InputFilter;
import android.text.InputType;
import android.text.TextWatcher;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewTreeObserver;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputMethodManager;
import android.widget.EditText;
import android.widget.FrameLayout;

import java.lang.ref.WeakReference;

import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.RuntimeGenerationBoundary;
import com.migo.runtime.internal.RuntimeScoped;

import org.json.JSONException;
import org.json.JSONObject;

/**
 * Manages soft keyboard lifecycle for a session.
 * <p>
 * Creates an overlay {@link EditText} to interact with the Android IME,
 * monitors keyboard height via layout changes, and dispatches events
 * (input, confirm, complete, heightChange) back to native via {@link NativeMethods}.
 * <p>
 * <ul>
 *   <li>defaultValue - initial text</li>
 *   <li>maxLength - max input length</li>
 *   <li>multiple - multiline input</li>
 *   <li>confirmHold - keep keyboard open on confirm</li>
 *   <li>confirmType - IME action: "done", "next", "search", "go", "send"</li>
 *   <li>keyboardType - input type: "text", "number", "digit", "email", "url", "phone"</li>
 * </ul>
 *
 * @hide
 */
public class KeyboardManager implements RuntimeScoped {

    private static final String TAG = "KeyboardManager";
    private static final int INPUT_BAR_HORIZONTAL_MARGIN_DP = 0;
    private static final int INPUT_BAR_VERTICAL_PADDING_DP = 12;
    private static final int INPUT_BAR_HORIZONTAL_PADDING_DP = 14;
    private static final int INPUT_BAR_MIN_HEIGHT_DP = 44;

    private final int sessionId;
    private final WeakReference<Activity> activityRef;
    private final Handler mainHandler;

    private EditText editText;
    private boolean confirmHold;
    private boolean isShowing;
    private int lastKeyboardHeight;

    private ViewTreeObserver.OnGlobalLayoutListener layoutListener;

    private Activity getActivity() {
        return activityRef.get();
    }

    /**
     * The runtime this keyboard belongs to.
     *
     * <p>Captured once, here, and stamped on every event this manager reports.
     * Reading the current generation at report time instead would always match,
     * and the engine's comparison would drop nothing — which is exactly the
     * failure that reads as working.
     */
    private final RuntimeGenerationBoundary.Token token;

    @Override
    public RuntimeGenerationBoundary.Token runtimeToken() {
        return token;
    }

    public KeyboardManager(int sessionId, Activity activity) {
        this.sessionId = sessionId;
        this.token = RuntimeGenerationBoundary.acquire(sessionId);
        this.activityRef = new WeakReference<>(activity);
        this.mainHandler = new Handler(Looper.getMainLooper());
    }

    /**
     * Show the soft keyboard with the given options JSON.
     *
     * @param optionsJson JSON string with showKeyboard options
     */
    public void show(String optionsJson) {
        mainHandler.post(() -> showOnUiThread(optionsJson));
    }

    /**
     * Hide the soft keyboard.
     */
    public void hide() {
        mainHandler.post(this::hideOnUiThread);
    }

    /**
     * Update the current text value programmatically.
     *
     * @param value New text value
     */
    public void updateValue(String value) {
        mainHandler.post(() -> {
            if (editText != null && value != null) {
                editText.removeTextChangedListener(textWatcher);
                editText.setText(value);
                editText.setSelection(value.length());
                editText.addTextChangedListener(textWatcher);
            }
        });
    }

    /**
     * Release all resources. Called on session shutdown.
     */
    public void destroy() {
        mainHandler.post(() -> {
            hideOnUiThread();
            stopKeyboardHeightMonitoring();
            removeEditText();
        });
    }

    // ==================== Internal ====================

    private void showOnUiThread(String optionsJson) {
        String defaultValue = "";
        int maxLength = 140;
        boolean multiple = false;
        confirmHold = false;
        String confirmType = "done";
        String keyboardType = "text";

        if (optionsJson != null) {
            try {
                JSONObject opts = new JSONObject(optionsJson);
                defaultValue = opts.optString("defaultValue", "");
                maxLength = opts.optInt("maxLength", 140);
                multiple = opts.optBoolean("multiple", false);
                confirmHold = opts.optBoolean("confirmHold", false);
                confirmType = opts.optString("confirmType", "done");
                keyboardType = opts.optString("keyboardType", "text");
            } catch (JSONException ignored) {
            }
        }

        ensureEditText();
        if (editText == null) return;

        // Input type
        int inputType = mapInputType(keyboardType, multiple);
        editText.setInputType(inputType);

        // Max length
        if (maxLength > 0) {
            editText.setFilters(new InputFilter[]{new InputFilter.LengthFilter(maxLength)});
        } else {
            editText.setFilters(new InputFilter[0]);
        }

        // IME action
        int imeAction = mapImeAction(confirmType);
        editText.setImeOptions(imeAction | EditorInfo.IME_FLAG_NO_FULLSCREEN);
        editText.setSingleLine(!multiple);
        editText.setMaxLines(multiple ? 4 : 1);
        editText.setHorizontallyScrolling(!multiple);

        // Default value
        editText.removeTextChangedListener(textWatcher);
        editText.setText(defaultValue);
        editText.setSelection(defaultValue.length());
        editText.addTextChangedListener(textWatcher);

        // Show
        updateInputBarBottomMargin(0);
        editText.setVisibility(View.VISIBLE);
        editText.bringToFront();
        editText.requestFocus();
        isShowing = true;

        InputMethodManager imm = getImm();
        if (imm != null) {
            imm.showSoftInput(editText, InputMethodManager.SHOW_IMPLICIT);
        }

        startKeyboardHeightMonitoring();
    }

    private void hideOnUiThread() {
        if (!isShowing) return;
        isShowing = false;

        if (editText != null) {
            String value = editText.getText().toString();

            InputMethodManager imm = getImm();
            if (imm != null) {
                imm.hideSoftInputFromWindow(editText.getWindowToken(), 0);
            }

            editText.setVisibility(View.GONE);
            editText.clearFocus();
            updateInputBarBottomMargin(0);

            NativeMethods.onKeyboardComplete(sessionId, token.generation(), value);
        }

        // The keyboard leaves after this, as the input method animates it away:
        // its height is still reported until it reaches 0 (the layout listener
        // stops itself there), so content hears it go. One that is already 0 --
        // a floating keyboard, a hardware one -- has nothing more to say.
        if (lastKeyboardHeight == 0) {
            stopKeyboardHeightMonitoring();
        }
    }

    private void ensureEditText() {
        if (editText != null) return;

        Activity activity = getActivity();
        if (activity == null) return;
        editText = new EditText(activity);
        editText.setVisibility(View.GONE);
        editText.setBackgroundColor(0xFFFFFFFF);
        editText.setTextColor(0xFF111111);
        editText.setHintTextColor(0xFF999999);
        editText.setCursorVisible(true);
        editText.setPadding(
                dpToPx(INPUT_BAR_HORIZONTAL_PADDING_DP),
                dpToPx(INPUT_BAR_VERTICAL_PADDING_DP),
                dpToPx(INPUT_BAR_HORIZONTAL_PADDING_DP),
                dpToPx(INPUT_BAR_VERTICAL_PADDING_DP)
        );
        editText.setMinHeight(dpToPx(INPUT_BAR_MIN_HEIGHT_DP));
        editText.setGravity(Gravity.START | Gravity.CENTER_VERTICAL);
        editText.setFocusable(true);
        editText.setFocusableInTouchMode(true);

        FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.WRAP_CONTENT
        );
        int horizontalMargin = dpToPx(INPUT_BAR_HORIZONTAL_MARGIN_DP);
        lp.leftMargin = horizontalMargin;
        lp.rightMargin = horizontalMargin;
        lp.gravity = Gravity.BOTTOM;

        View decorView = activity.getWindow().getDecorView();
        if (decorView instanceof FrameLayout) {
            ((FrameLayout) decorView).addView(editText, lp);
        } else {
            FrameLayout contentView = activity.findViewById(android.R.id.content);
            if (contentView != null) {
                contentView.addView(editText, lp);
            }
        }

        // Editor action listener for confirm
        editText.setOnEditorActionListener((v, actionId, event) -> {
            boolean multiple = (editText.getInputType() & InputType.TYPE_TEXT_FLAG_MULTI_LINE) != 0;
            if (!isConfirmAction(actionId, event, multiple)) {
                return false;
            }

            String value = editText.getText().toString();
            NativeMethods.onKeyboardConfirm(sessionId, token.generation(), value);
            if (!confirmHold) {
                hideOnUiThread();
            }
            return true;
        });
    }

    private void removeEditText() {
        if (editText != null) {
            if (editText.getParent() instanceof FrameLayout) {
                ((FrameLayout) editText.getParent()).removeView(editText);
            }
            editText = null;
        }
    }

    private final TextWatcher textWatcher = new TextWatcher() {
        @Override
        public void beforeTextChanged(CharSequence s, int start, int count, int after) {
        }

        @Override
        public void onTextChanged(CharSequence s, int start, int before, int count) {
        }

        @Override
        public void afterTextChanged(Editable s) {
            NativeMethods.onKeyboardInput(sessionId, token.generation(), s.toString());
        }
    };

    // ==================== Keyboard Height Monitoring ====================

    private void startKeyboardHeightMonitoring() {
        stopKeyboardHeightMonitoring();

        Activity activity = getActivity();
        if (activity == null) return;
        View decorView = activity.getWindow().getDecorView();
        layoutListener = () -> {
            Rect r = new Rect();
            decorView.getWindowVisibleDisplayFrame(r);
            int screenHeight = decorView.getRootView().getHeight();
            int keyboardHeight = Math.max(0, screenHeight - r.bottom);

            if (keyboardHeight != lastKeyboardHeight) {
                int previousKeyboardHeight = lastKeyboardHeight;
                lastKeyboardHeight = keyboardHeight;

                updateInputBarBottomMargin(keyboardHeight);

                // Convert physical pixels to CSS pixels (dp)
                float density = activity.getResources().getDisplayMetrics().density;
                double cssHeight = keyboardHeight / density;
                NativeMethods.onKeyboardHeightChange(sessionId, token.generation(), cssHeight);

                if (isShowing && previousKeyboardHeight > 0 && keyboardHeight == 0) {
                    completeBySystemDismiss();
                } else if (!isShowing && keyboardHeight == 0) {
                    // The field was closed and the keyboard is now gone.
                    stopKeyboardHeightMonitoring();
                }
            }
        };
        decorView.getViewTreeObserver().addOnGlobalLayoutListener(layoutListener);
    }

    private void stopKeyboardHeightMonitoring() {
        if (layoutListener != null) {
            Activity activity = getActivity();
            if (activity != null) {
                View decorView = activity.getWindow().getDecorView();
                decorView.getViewTreeObserver().removeOnGlobalLayoutListener(layoutListener);
            }
            layoutListener = null;
            lastKeyboardHeight = 0;
        }
    }

    private void completeBySystemDismiss() {
        if (!isShowing) {
            return;
        }
        isShowing = false;

        if (editText != null) {
            String value = editText.getText().toString();
            editText.setVisibility(View.GONE);
            editText.clearFocus();
            updateInputBarBottomMargin(0);
            NativeMethods.onKeyboardComplete(sessionId, token.generation(), value);
        }

        stopKeyboardHeightMonitoring();
    }

    private void updateInputBarBottomMargin(int keyboardHeightPx) {
        if (editText == null) {
            return;
        }
        if (!(editText.getLayoutParams() instanceof FrameLayout.LayoutParams)) {
            return;
        }
        FrameLayout.LayoutParams lp = (FrameLayout.LayoutParams) editText.getLayoutParams();
        int bottom = Math.max(0, keyboardHeightPx);
        if (lp.bottomMargin != bottom) {
            lp.bottomMargin = bottom;
            editText.setLayoutParams(lp);
        }
    }

    private int dpToPx(int dp) {
        Activity activity = getActivity();
        if (activity == null) return dp;
        float density = activity.getResources().getDisplayMetrics().density;
        return Math.round(dp * density);
    }

    private static boolean isConfirmAction(int actionId, KeyEvent event, boolean multiple) {
        switch (actionId) {
            case EditorInfo.IME_ACTION_DONE:
            case EditorInfo.IME_ACTION_GO:
            case EditorInfo.IME_ACTION_NEXT:
            case EditorInfo.IME_ACTION_SEARCH:
            case EditorInfo.IME_ACTION_SEND:
                return true;
            case EditorInfo.IME_NULL:
                return !multiple
                        && event != null
                        && event.getKeyCode() == KeyEvent.KEYCODE_ENTER
                        && event.getAction() == KeyEvent.ACTION_DOWN;
            default:
                return false;
        }
    }

    // ==================== Helpers ====================

    private InputMethodManager getImm() {
        Activity activity = getActivity();
        if (activity == null) return null;
        return (InputMethodManager) activity.getSystemService(Activity.INPUT_METHOD_SERVICE);
    }

    private static int mapInputType(String keyboardType, boolean multiple) {
        int base;
        switch (keyboardType) {
            case "number":
                base = InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_FLAG_SIGNED
                        | InputType.TYPE_NUMBER_FLAG_DECIMAL;
                break;
            case "digit":
                base = InputType.TYPE_CLASS_NUMBER;
                break;
            case "email":
                base = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS;
                break;
            case "url":
                base = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI;
                break;
            case "phone":
                base = InputType.TYPE_CLASS_PHONE;
                break;
            default: // "text"
                base = InputType.TYPE_CLASS_TEXT;
                break;
        }
        if (multiple) {
            base |= InputType.TYPE_TEXT_FLAG_MULTI_LINE;
        }
        return base;
    }

    private static int mapImeAction(String confirmType) {
        switch (confirmType) {
            case "next":
                return EditorInfo.IME_ACTION_NEXT;
            case "search":
                return EditorInfo.IME_ACTION_SEARCH;
            case "go":
                return EditorInfo.IME_ACTION_GO;
            case "send":
                return EditorInfo.IME_ACTION_SEND;
            default: // "done"
                return EditorInfo.IME_ACTION_DONE;
        }
    }
}
