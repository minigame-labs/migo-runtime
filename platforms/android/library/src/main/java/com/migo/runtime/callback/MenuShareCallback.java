package com.migo.runtime.callback;

/**
 * Where the game's answer goes when the player picks a share item from your
 * menu: see {@link com.migo.runtime.GameSession#requestMenuShare}.
 * <p>
 * Called exactly once per request, on the runtime's host thread -- with
 * {@code null} when nothing in the game answered, or when the game's runtime
 * restarted first. Then share with your own defaults: the game's name, its icon.
 */
public interface MenuShareCallback {

    /** "Share": the game's {@code onShareAppMessage} answers. */
    String MENU_SHARE_APP_MESSAGE = "shareAppMessage";
    /** "Share to moments": the game's {@code onShareTimeline} answers. */
    String MENU_SHARE_TIMELINE = "shareTimeline";
    /** "Add to favorites": the game's {@code onAddToFavorites} answers. */
    String MENU_ADD_TO_FAVORITES = "addToFavorites";

    /**
     * @param content what to share, or null to share your defaults
     */
    void onShareContent(MenuShareContent content);
}
