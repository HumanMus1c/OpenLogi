// OpenLogi Frontmost Window — GNOME Shell extension.
//
// Exports a tiny D-Bus service that returns the WM_CLASS of the currently
// focused window and signals focus changes. OpenLogi's `gnome_shell` backend
// uses this to drive per-app mouse-profile switching on GNOME Wayland, where
// the focused window is otherwise not visible to ordinary clients.
//
// It also keeps Actions Ring surfaces out of GNOME's window animations so
// their initial pointer coordinates stay valid, and remembers where the user
// left the main window and the Flow Indicator card (GNOME does not let a
// Wayland client place its own toplevel). No window contents or input are
// read. ESM module style; targets GNOME Shell 45+.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import {Extension, InjectionManager} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {withoutRingAnimation} from './ringAnimation.js';
import {TRANSIENT_APP_IDS, WindowStateManager} from './windowState.js';

const DBUS_NAME = 'org.openlogi.Frontmost';
const DBUS_PATH = '/org/openlogi/Frontmost';
const DBUS_INTERFACE = `
<node>
  <interface name="org.openlogi.Frontmost">
    <method name="GetFocusedWmClass">
      <arg type="s" direction="out" name="wmClass"/>
    </method>
    <signal name="FocusedWmClassChanged">
      <arg type="s" name="wmClass"/>
    </signal>
  </interface>
</node>`;

export default class OpenLogiFrontmostExtension extends Extension {
    enable() {
        this._dbus = Gio.DBusExportedObject.wrapJSObject(DBUS_INTERFACE, this);
        this._dbus.export(Gio.DBus.session, DBUS_PATH);
        this._nameId = Gio.bus_own_name_on_connection(
            Gio.DBus.session,
            DBUS_NAME,
            Gio.BusNameOwnerFlags.NONE,
            null,
            null);
        this._focusChangedId = global.display.connect(
            'notify::focus-window',
            () => this._emitFocusedWmClassChanged());

        this._ringAnimations = new InjectionManager();
        this._ringAnimations.overrideMethod(
            Main.wm, '_shouldAnimateActor', withoutRingAnimation);

        // GNOME's window-attention handler announces every demanding window as
        // "X is ready". OpenLogi's transient OSD/notice/ring surfaces are
        // created in the background and are not user-facing windows; keep them
        // out of that notification (and the task list).
        this._attention = new InjectionManager();
        const attention = Main.wm?._windowAttentionHandler;
        if (attention) {
            this._attention.overrideMethod(attention, '_windowDemandsAttention', original =>
                function (window) {
                    if (TRANSIENT_APP_IDS.has(window.get_wm_class()))
                        return;
                    return original.call(this, window);
                });
        }

        this._windowState = new WindowStateManager();
        this._windowState.enable();
    }

    disable() {
        this._windowState?.disable();
        this._windowState = null;
        this._attention?.clear();
        this._attention = null;
        this._ringAnimations?.clear();
        this._ringAnimations = null;
        if (this._focusChangedId) {
            global.display.disconnect(this._focusChangedId);
            this._focusChangedId = 0;
        }
        if (this._nameId) {
            Gio.bus_unown_name(this._nameId);
            this._nameId = 0;
        }
        if (this._dbus) {
            this._dbus.unexport();
            this._dbus = null;
        }
    }

    // D-Bus method org.openlogi.Frontmost.GetFocusedWmClass.
    GetFocusedWmClass() {
        const win = global.display.focus_window;
        if (!win)
            return '';
        return win.get_wm_class() || '';
    }

    _emitFocusedWmClassChanged() {
        if (this._dbus) {
            this._dbus.emit_signal(
                'FocusedWmClassChanged',
                new GLib.Variant('(s)', [this.GetFocusedWmClass()]));
        }
    }
}
