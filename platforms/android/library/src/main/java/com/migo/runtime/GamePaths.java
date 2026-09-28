package com.migo.runtime;

import java.io.File;
import java.util.Collections;
import java.util.HashSet;
import java.util.Set;
import java.util.function.IntPredicate;
import java.util.regex.Pattern;

/**
 * Manages isolated file paths for a specific game.
 * <p>
 * Each game has its own sandboxed directories:
 * <ul>
 *   <li>{@code /user} → User data (saves, preferences) - persistent</li>
 *   <li>{@code /cache} → Cache files - may be cleared by system</li>
 *   <li>{@code /code} → Game code - read only</li>
 *   <li>{@code /tmp} → Temporary files - private to one session, cleared when
 *       that session ends</li>
 * </ul>
 * <p>
 * {@link #getCacheDir()} is the per-game cache root rather than the directory
 * {@code /cache} resolves to; see its documentation.
 * <p>
 * Every path here is derived the same way as
 * {@code shared::vfs::game_paths::GamePaths} in the engine, and the two must move
 * together. They are two computations of one layout: the engine resolves
 * {@code /tmp} for the content that writes it, and this class is what deletes it,
 * so a change on one side alone moves what is written without moving what is
 * removed — or the reverse.
 *
 */
public final class GamePaths {

    // Game ID validation: lower-case alphanumeric, underscore, hyphen, 1-64
    // chars. This rule is `shared::vfs::game_paths::validate_game_id` in the
    // engine, and the two must move together: the engine refuses a session
    // whose id this class accepted. Both sides are pinned by the shared table
    // in engine/crates/shared/src/vfs/game-id-vectors.txt.
    private static final Pattern GAME_ID_PATTERN = Pattern.compile("^[a-z0-9_-]{1,64}$");

    /**
     * Ids that name a device rather than a file on Windows.
     *
     * <p>Refused here too, on Android, so that one id is valid on every
     * platform the runtime targets or on none of them. A host that ships the
     * same catalogue to a Windows build would otherwise find these titles
     * unable to create their storage directory there and nowhere else.
     */
    private static final Set<String> RESERVED_GAME_IDS = reservedGameIds();

    private static Set<String> reservedGameIds() {
        Set<String> reserved = new HashSet<>();
        reserved.add("con");
        reserved.add("prn");
        reserved.add("aux");
        reserved.add("nul");
        for (char digit = '0'; digit <= '9'; digit++) {
            reserved.add("com" + digit);
            reserved.add("lpt" + digit);
        }
        return Collections.unmodifiableSet(reserved);
    }

    private static final String MIGO_GAMES_DIR = "migo/games";

    /** Parent of the per-session temporary directories. */
    private static final String TEMP_DIR = "tmp";

    private final String gameId;
    private final File userDataDir;
    private final File cacheDir;
    private final File codeDir;
    private final File tempRoot;
    private final File tempDir;

    /**
     * Create game paths from RuntimeConfig, game ID and session ID.
     *
     * @param config    RuntimeConfig with base directories
     * @param gameId    Unique game identifier
     * @param sessionId The session these paths belong to, which is what keeps
     *                  {@code /tmp} private to it. Two concurrently live sessions
     *                  of the same game share {@code /user} and {@code /cache} on
     *                  purpose — that is one game's save data — but {@code /tmp}
     *                  lasts for a session, and sharing it made the first session
     *                  to close delete the other's live files mid-write.
     * @throws IllegalArgumentException if gameId is invalid
     */
    public GamePaths(RuntimeConfig config, String gameId, int sessionId) {
        if (!isValidGameId(gameId)) {
            throw new IllegalArgumentException(
                "Invalid gameId: must be 1-64 lower-case alphanumeric characters, underscore or hyphen, and not a reserved device name");
        }
        this.gameId = gameId;

        // Persistent storage: filesDir/migo/games/{gameId}/
        File filesBase = new File(config.getFilesDir(), MIGO_GAMES_DIR + "/" + gameId);
        this.userDataDir = new File(filesBase, "user_data");
        this.codeDir = new File(filesBase, "code");

        // Cache storage: cacheDir/migo/games/{gameId}/
        File cacheBase = new File(config.getCacheDir(), MIGO_GAMES_DIR + "/" + gameId);
        this.cacheDir = cacheBase;
        // A directory level rather than a name suffix, so a sweep of abandoned
        // temp directories is confined to one subtree and cannot reach the
        // subpackage install store or the staging directories beside it.
        this.tempRoot = new File(cacheBase, TEMP_DIR);
        this.tempDir = new File(tempRoot, Integer.toString(sessionId));
    }

    /**
     * Validate a game ID.
     *
     * <p>Lower case only, and never a reserved device name. The id becomes one
     * path component per game and that component is the storage isolation
     * boundary, so it has to mean the same thing on a case-insensitive
     * filesystem as it does here: {@code PuzzleQuest} and {@code puzzlequest}
     * are two games on Android and one directory on Windows.
     *
     * @param gameId The game ID to validate
     * @return true if valid
     */
    public static boolean isValidGameId(String gameId) {
        return gameId != null
                && GAME_ID_PATTERN.matcher(gameId).matches()
                && !RESERVED_GAME_IDS.contains(gameId);
    }

    /**
     * Get the game ID.
     */
    public String getGameId() {
        return gameId;
    }

    /**
     * Get the user data directory (persistent).
     * <p>
     * Use for saves, preferences, user-generated content.
     * Maps to virtual path: /user
     */
    public File getUserDataDir() {
        return userDataDir;
    }

    /**
     * Get the cache directory.
     * <p>
     * Use for downloaded resources, decoded images, etc.
     * May be cleared by the system when storage is low.
     * <p>
     * This is the per-game cache root, and the game does not see it directly:
     * it also holds runtime state — the subpackage install store and record, and
     * install staging directories — so the virtual path {@code /cache} is mapped
     * to a dedicated subdirectory of it instead. A game that could write the
     * install record would decide which package bytes a later session mounts as
     * {@code /code}.
     */
    public File getCacheDir() {
        return cacheDir;
    }

    /**
     * Get the code directory (read-only for game).
     * <p>
     * Contains the game's JavaScript code and assets.
     * After the first successful signed launch, Migo clears Unix write bits on
     * the active tree. A trusted installer must stage a complete sibling tree
     * for an update; it must serialize deployment against session start, stop
     * every session for this game, and publish only a complete tree. It must not
     * overwrite the active tree in place or assume {@link File#renameTo(File)}
     * atomically replaces a non-empty directory on API 26. Use a recoverable
     * whole-tree transaction and finish recovery before the next start. A
     * detached old tree may have its write bits restored before cleanup.
     * Maps to virtual path: /code
     */
    public File getCodeDir() {
        return codeDir;
    }

    /**
     * Get the temporary directory.
     * <p>
     * Private to this session and cleared when it ends, so a second live session
     * of the same game neither reads these files nor loses them.
     * Maps to virtual path: /tmp
     */
    public File getTempDir() {
        return tempDir;
    }

    /**
     * Ensure the runtime-managed directories exist before starting the game.
     * The code directory is created by game deployment or extraction logic and
     * is intentionally not created here.
     */
    public void ensureDirectories() {
        userDataDir.mkdirs();
        cacheDir.mkdirs();
        tempDir.mkdirs();
        // codeDir is created by download/extraction logic
    }

    /**
     * Clean up this session's temporary files.
     * Call this when the session ends.
     * <p>
     * Scoped to this session's directory: it used to be the whole per-game temp
     * directory, which a second live session of the same game was still writing.
     */
    public void cleanupTemp() {
        deleteRecursive(tempDir);
        tempDir.mkdirs();
    }

    /**
     * Remove the temp directories of sessions that are no longer live.
     *
     * <p>A session's temp directory is removed by its own teardown, so what is
     * left here belongs to a session whose process died before it could run —
     * app kills are routine on Android — or to a build that wrote {@code /tmp}
     * before it was split per session, whose files sit directly in this
     * directory. Neither has an owner that will ever come back for it.
     *
     * <p>Called before this session creates its own directory, which is also what
     * makes it the guarantee that {@code /tmp} starts empty: this session's id is
     * not live yet, so a directory left behind by a dead session that happened to
     * hold the same id is swept as abandoned rather than inherited. Session ids
     * are a per-process counter, so after a restart they are handed out again from
     * the beginning and that collision is the common case, not a remote one.
     *
     * <p>{@code isLive} decides ownership rather than a timestamp or a name
     * pattern, because only liveness distinguishes "abandoned" from "in use by
     * someone else right now". Passing it in keeps this class free of the session
     * registry and lets the sweep be tested without one.
     *
     * @param isLive whether a session id still has a live session behind it
     */
    void sweepAbandonedTemp(IntPredicate isLive) {
        File[] entries = tempRoot.listFiles();
        if (entries == null) return;
        for (File entry : entries) {
            Integer owner = temporaryDirectoryOwner(entry);
            if (owner != null && isLive.test(owner)) continue;
            deleteRecursive(entry);
        }
    }

    /**
     * The session id {@code entry} belongs to, or {@code null} if it belongs to no
     * session at all — a loose file from before {@code /tmp} was split per session,
     * or a name this class did not write.
     */
    private static Integer temporaryDirectoryOwner(File entry) {
        if (!entry.isDirectory()) return null;
        try {
            return Integer.valueOf(entry.getName());
        } catch (NumberFormatException notASessionDirectory) {
            return null;
        }
    }

    /**
     * Delete all game data (uninstall).
     * <p>
     * Removes both persistent and cache data.
     */
    public void deleteAll() {
        deleteRecursive(userDataDir.getParentFile()); // Delete filesDir/.../gameId/
        deleteRecursive(cacheDir);                     // Delete cacheDir/.../gameId/
    }

    /**
     * Get the total size of user data in bytes.
     */
    public long getUserDataSize() {
        return getDirectorySize(userDataDir);
    }

    /**
     * Get the total size of cache in bytes.
     */
    public long getCacheSize() {
        return getDirectorySize(cacheDir);
    }

    private static void deleteRecursive(File file) {
        if (file == null || !file.exists()) return;
        if (file.isDirectory()) {
            // Signed launch seals the active code tree. Trusted uninstall must
            // restore owner traversal/mutation bits before removing children.
            file.setWritable(true, true);
            file.setExecutable(true, true);
            File[] children = file.listFiles();
            if (children != null) {
                for (File child : children) {
                    deleteRecursive(child);
                }
            }
        }
        file.delete();
    }

    private static long getDirectorySize(File dir) {
        if (dir == null || !dir.exists()) return 0;
        if (dir.isFile()) return dir.length();

        long size = 0;
        File[] children = dir.listFiles();
        if (children != null) {
            for (File child : children) {
                size += getDirectorySize(child);
            }
        }
        return size;
    }

    @Override
    public String toString() {
        return "GamePaths{" +
                "gameId='" + gameId + '\'' +
                '}';
    }
}
