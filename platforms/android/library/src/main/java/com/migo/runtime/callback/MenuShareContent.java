package com.migo.runtime.callback;

/**
 * What the game answered for one share item of your menu. A field the game did
 * not set is empty (or its default); which fields apply depends on the item.
 * An image is an http(s) URL or a real file path resolved inside the game's
 * sandbox, readable for the rest of the session.
 */
public final class MenuShareContent {
    /** Share title, empty for the game's name. */
    public final String title;
    /** Share image, empty for your default (a frame of the game, its icon). */
    public final String imageUrl;
    /** Query string the game opened from this share receives, without a leading {@code ?}. */
    public final String query;
    /** Platform image id; meaningful only to a host that resolves such ids. */
    public final String imageUrlId;
    /** "Share": whether a share made from a group's toolbar goes back to that group. */
    public final boolean toCurrentGroup;
    /** The independent subpackage the share opens, empty when the game named none. */
    public final String path;
    /** "Share to moments": the preview image, empty for your default. */
    public final String imagePreviewUrl;
    /** "Share to moments": the preview image's platform id. */
    public final String imagePreviewUrlId;
    /** "Add to favorites": whether a kept item may not be forwarded. */
    public final boolean disableForward;

    public MenuShareContent(
            String title,
            String imageUrl,
            String query,
            String imageUrlId,
            boolean toCurrentGroup,
            String path,
            String imagePreviewUrl,
            String imagePreviewUrlId,
            boolean disableForward) {
        this.title = title;
        this.imageUrl = imageUrl;
        this.query = query;
        this.imageUrlId = imageUrlId;
        this.toCurrentGroup = toCurrentGroup;
        this.path = path;
        this.imagePreviewUrl = imagePreviewUrl;
        this.imagePreviewUrlId = imagePreviewUrlId;
        this.disableForward = disableForward;
    }
}
