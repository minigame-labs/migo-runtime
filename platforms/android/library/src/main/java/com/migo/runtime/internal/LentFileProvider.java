package com.migo.runtime.internal;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.Context;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import android.system.ErrnoException;
import android.system.Os;
import android.webkit.MimeTypeMap;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileNotFoundException;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicLong;

/**
 * Lends a game's file to another app -- a gallery showing {@code previewImage} --
 * read-only, through a {@code content://} URI the lending intent grants.
 *
 * <p>A file is lent as a link (or, across volumes, a copy) in a directory of this
 * provider's own, so the loan does not depend on the original: a path the runtime
 * gave the SDK is only promised until the request is answered, and a viewer opens
 * the URI after that. Loans are dropped with their session.
 *
 * <p>It also lends a camera app the file a capture is to be written to, so a
 * photo or video taken for {@code chooseImage} / {@code chooseMedia} goes to the
 * game rather than into the player's gallery.
 *
 * <p>Not exported: only an intent carrying a URI grant reaches a file, and only
 * that one; only a capture target is writable.
 *
 * @hide
 */
public final class LentFileProvider extends ContentProvider {

    private static final String AUTHORITY_SUFFIX = ".migo.lent";
    private static final String LENT_DIR = "migo-lent";

    private static final class Loan {
        final int sessionId;
        final File file;
        final String displayName;
        final boolean writable;

        Loan(int sessionId, File file, String displayName, boolean writable) {
            this.sessionId = sessionId;
            this.file = file;
            this.displayName = displayName;
            this.writable = writable;
        }
    }

    private static final ConcurrentHashMap<String, Loan> sLoans = new ConcurrentHashMap<>();
    private static final AtomicLong sNextToken = new AtomicLong(1);

    /**
     * A URI another app can read {@code file} through. Runs file I/O: call it off
     * the main thread.
     */
    public static Uri lend(Context context, int sessionId, File file) throws IOException {
        if (!file.isFile()) {
            throw new FileNotFoundException(file.getPath());
        }
        File dir = new File(new File(context.getCacheDir(), LENT_DIR), Integer.toString(sessionId));
        if (!dir.isDirectory() && !dir.mkdirs()) {
            throw new IOException("cannot create " + dir);
        }
        String token = Long.toString(sNextToken.getAndIncrement(), 36);
        File loaned = new File(dir, token + extensionOf(file.getName()));
        try {
            Os.link(file.getAbsolutePath(), loaned.getAbsolutePath());
        } catch (ErrnoException notLinkable) {
            copy(file, loaned);
        }
        sLoans.put(token, new Loan(sessionId, loaned, file.getName(), false));
        return uriFor(context, token, file.getName());
    }

    /**
     * A URI a camera app can write {@code target} through. The loan ends with
     * {@link #forget}, once the capture has returned.
     */
    public static Uri lendForCapture(Context context, int sessionId, File target) throws IOException {
        if (!target.isFile() && !target.createNewFile()) {
            throw new IOException("cannot create " + target);
        }
        String token = Long.toString(sNextToken.getAndIncrement(), 36);
        sLoans.put(token, new Loan(sessionId, target, target.getName(), true));
        return uriFor(context, token, target.getName());
    }

    /** End one loan. The file stays where it is. */
    public static void forget(Uri uri) {
        List<String> segments = uri.getPathSegments();
        if (!segments.isEmpty()) {
            sLoans.remove(segments.get(0));
        }
    }

    private static Uri uriFor(Context context, String token, String name) {
        return new Uri.Builder()
                .scheme("content")
                .authority(context.getPackageName() + AUTHORITY_SUFFIX)
                .appendPath(token)
                .appendPath(name)
                .build();
    }

    /** End every loan the session made, removing the links and copies. Runs file I/O. */
    public static void forgetSession(Context context, int sessionId) {
        sLoans.values().removeIf(loan -> loan.sessionId == sessionId);
        File dir = new File(new File(context.getCacheDir(), LENT_DIR), Integer.toString(sessionId));
        File[] files = dir.listFiles();
        if (files != null) {
            for (File file : files) {
                //noinspection ResultOfMethodCallIgnored
                file.delete();
            }
        }
        //noinspection ResultOfMethodCallIgnored
        dir.delete();
    }

    private static String extensionOf(String name) {
        int dot = name.lastIndexOf('.');
        return dot > 0 ? name.substring(dot) : "";
    }

    private static void copy(File from, File to) throws IOException {
        try (InputStream in = new FileInputStream(from);
             OutputStream out = new FileOutputStream(to)) {
            byte[] buffer = new byte[64 * 1024];
            int read;
            while ((read = in.read(buffer)) > 0) {
                out.write(buffer, 0, read);
            }
        }
    }

    private static Loan loanFor(Uri uri) throws FileNotFoundException {
        List<String> segments = uri.getPathSegments();
        Loan loan = segments.isEmpty() ? null : sLoans.get(segments.get(0));
        if (loan == null) {
            throw new FileNotFoundException(uri.toString());
        }
        return loan;
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        Loan loan = loanFor(uri);
        if ("r".equals(mode)) {
            return ParcelFileDescriptor.open(loan.file, ParcelFileDescriptor.MODE_READ_ONLY);
        }
        if (!loan.writable) {
            throw new SecurityException("lent files are read-only");
        }
        return ParcelFileDescriptor.open(loan.file, ParcelFileDescriptor.parseMode(mode));
    }

    @Override
    public String getType(Uri uri) {
        String name = uri.getLastPathSegment();
        String ext = name == null ? "" : extensionOf(name);
        if (ext.isEmpty()) {
            return "application/octet-stream";
        }
        String mime = MimeTypeMap.getSingleton()
                .getMimeTypeFromExtension(ext.substring(1).toLowerCase(Locale.ROOT));
        return mime != null ? mime : "application/octet-stream";
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] selectionArgs,
                        String sortOrder) {
        Loan loan;
        try {
            loan = loanFor(uri);
        } catch (FileNotFoundException gone) {
            return null;
        }
        String[] columns = projection != null
                ? projection
                : new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE};
        MatrixCursor cursor = new MatrixCursor(columns, 1);
        Object[] row = new Object[columns.length];
        for (int i = 0; i < columns.length; i++) {
            if (OpenableColumns.DISPLAY_NAME.equals(columns[i])) {
                row[i] = loan.displayName;
            } else if (OpenableColumns.SIZE.equals(columns[i])) {
                row[i] = loan.file.length();
            }
        }
        cursor.addRow(row);
        return cursor;
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        throw new UnsupportedOperationException("read-only");
    }

    @Override
    public int delete(Uri uri, String selection, String[] selectionArgs) {
        throw new UnsupportedOperationException("read-only");
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] selectionArgs) {
        throw new UnsupportedOperationException("read-only");
    }
}
