/*
 * Does GTK 4 let a child widget own a native surface?
 *
 * The Host Kit's Direct Surface contract needs one: Migo presents into a native
 * target the host places in its layout, and the toolkit clips and composites it
 * with the surrounding widgets. Qt Widgets allows exactly that through
 * WA_NativeWindow. This probe answers the same question for GTK 4, because the
 * roadmap's ordering depends on the answer and a documentation reading is not
 * evidence.
 *
 * The answer today is no: only widgets implementing GtkNative have a
 * GdkSurface, and that is the toplevel (plus popovers), not an arbitrary child.
 * GtkSocket and GtkPlug, which GTK 3 offered for embedding foreign windows, are
 * gone. Presenting into the toplevel's surface instead would put Migo over the
 * whole window with no clipping or z-order -- the child-window overlay the
 * architecture forbids as a fallback.
 *
 * So this is a gate, not a curiosity: the day GTK 4 grows native child surfaces
 * this probe fails, and that failure is the signal that a GTK Host Kit became
 * possible without the zero-copy texture and fence contract.
 *
 * Exit codes:
 *   0  the documented answer still holds (no native child surface)
 *   1  the answer changed -- re-read the roadmap
 *   2  the probe could not run
 */
#include <gtk/gtk.h>

#ifdef GDK_WINDOWING_X11
#include <gdk/x11/gdkx.h>
#endif

static int g_exit_code = 2;

static void on_activate(GtkApplication *app, gpointer user_data) {
    (void)user_data;
    GtkWidget *window = gtk_application_window_new(app);
    GtkWidget *box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 0);
    GtkWidget *child = gtk_drawing_area_new();

    gtk_box_append(GTK_BOX(box), child);
    gtk_window_set_child(GTK_WINDOW(window), box);
    gtk_widget_set_visible(window, TRUE);

    const gboolean toplevel_is_native = GTK_IS_NATIVE(window);
    const gboolean box_is_native = GTK_IS_NATIVE(box);
    const gboolean child_is_native = GTK_IS_NATIVE(child);
    GtkNative *nearest = gtk_widget_get_native(child);

    g_print("gtk4-native-surface-probe: gtk %d.%d.%d\n", gtk_get_major_version(),
            gtk_get_minor_version(), gtk_get_micro_version());
    g_print("  toplevel is GtkNative        : %s\n", toplevel_is_native ? "yes" : "no");
    g_print("  intermediate box is GtkNative: %s\n", box_is_native ? "yes" : "no");
    g_print("  leaf child is GtkNative      : %s\n", child_is_native ? "yes" : "no");
    g_print("  nearest native of the child  : %s\n",
            nearest == GTK_NATIVE(window) ? "the toplevel window" : "not the toplevel");

    if (!toplevel_is_native) {
        g_printerr("gtk4-native-surface-probe: the toplevel has no surface either; "
                   "the probe is measuring something other than what it claims\n");
        g_exit_code = 2;
    } else if (box_is_native || child_is_native) {
        g_printerr("gtk4-native-surface-probe: a child widget now owns a native surface. "
                   "GTK 4 may have gained the Direct Surface path, so this "
                   "probe's assumption needs revisiting before treating this "
                   "as a failure\n");
        g_exit_code = 1;
    } else {
        g_exit_code = 0;
    }

    g_application_quit(G_APPLICATION(app));
}

int main(int argc, char **argv) {
    GtkApplication *app =
        gtk_application_new("org.migo.gtk4-native-surface-probe", G_APPLICATION_DEFAULT_FLAGS);
    g_signal_connect(app, "activate", G_CALLBACK(on_activate), NULL);
    const int run_status = g_application_run(G_APPLICATION(app), argc, argv);
    if (run_status != 0) return 2;
    return g_exit_code;
}
