# Sourced by AppRun after the GTK hook. this_dir is set by AppRun.
# Keep every command successful: AppRun uses set -e.

# The GTK hook exports APPDIR. Fall back to the AppImage mount.
export APPDIR="${APPDIR:-$this_dir}"

# Sessions export GTK_MODULES (Plasma: colorreload, window-decorations).
# Those .so files are built against the host GTK. Clear the variable so
# GTK does not load them. A desktop settings.ini can still name modules;
# a missing file is a Gtk-Message, which is the safe outcome.
export GTK_MODULES=

# Bundled GLib is Ubuntu 22.04's 2.72. libgio's compiled-in module
# directory is the host path /usr/lib/.../gio/modules, so a newer
# distro's gvfs is dlopen'd into this process and fails
# (g_task_set_static_name arrived in GLib 2.76). An empty directory
# keeps GIO on its local file backend. The task manager does not use gvfs.
export GIO_MODULE_DIR="${APPDIR}/usr/lib/gio/modules"

# at-spi2-atk 2.38 (bundled) registers with the session accessibility
# bus. check_envvar() enables the bridge unless this value is 1.
# A newer bus answers GetDeviceEvents with a signature the bridge
# does not know; it logs "unknown signature" and the process
# segfaults about one D-Bus timeout later. The egui window does not
# go through ATK. The tray is a GTK menu and works without the bridge.
export NO_AT_BRIDGE=1
