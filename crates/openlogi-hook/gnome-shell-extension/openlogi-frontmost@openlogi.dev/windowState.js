// Window placement memory for GNOME Wayland.
//
// GNOME does not let ordinary clients position a toplevel, and it does not
// implement wlr-layer-shell, so the app cannot restore the main window or the
// Flow Indicator card itself. The shell can: this manager tracks the two
// OpenLogi surfaces, moves each one to its saved frame after it opens, saves
// the frame the user left it at, and keeps the card above other windows when
// the app's display mode asks for it.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

import {
    parseDisplayMode,
    parseFlowPosition,
    parseState,
    serializeState,
    windowKind,
} from './windowStatePolicy.js';

const RESTORE_DELAY_MS = 350;
const RESTORE_FALLBACK_MS = 1600;
const SAVE_DELAY_MS = 300;
const POLL_PERIOD_MS = 1000;

// Transient OpenLogi surfaces are pop-up windows the app creates and removes
// quickly. GNOME's window-attention handler announces them as "Window is
// ready" (with the empty OS title it shows as `"null" is ready`), and the
// task list shows them. Keep them out of both.
export const TRANSIENT_APP_IDS = new Set([
    'openlogi-lock-key-osd',
    'openlogi-notice',
    'openlogi-action-ring',
]);

function configDir() {
    return GLib.build_filenamev([GLib.get_user_config_dir(), 'openlogi']);
}

function statePath() {
    return GLib.build_filenamev([configDir(), 'gnome-window-state.json']);
}

function configPath() {
    return GLib.build_filenamev([configDir(), 'config.toml']);
}

function flowPositionPath() {
    return GLib.build_filenamev([configDir(), 'flow-indicator.toml']);
}

function readText(path) {
    try {
        const [ok, contents] = Gio.File.new_for_path(path).load_contents(null);
        return ok ? new TextDecoder().decode(contents) : null;
    } catch {
        return null;
    }
}

function writeText(path, text) {
    try {
        GLib.mkdir_with_parents(GLib.path_get_dirname(path), 0o755);
        Gio.File.new_for_path(path).replace_contents(
            new TextEncoder().encode(text),
            null,
            false,
            Gio.FileCreateFlags.NONE,
            null);
        return true;
    } catch (error) {
        console.log(`OpenLogi window state: could not write ${path}: ${error}`);
        return false;
    }
}

export class WindowStateManager {
    constructor() {
        this._windows = new Map();
        this._state = null;
        this._mainTaken = false;
        this._ready = new Set();
        this._lastFrame = new Map();
        this._restoreTimers = new Set();
        this._pollTimer = 0;
        this._saveTimer = 0;
        this._displayMode = 'normal';
        this._windowCreatedId = 0;
        this._configMonitor = null;
        this._configChangedId = 0;
    }

    enable() {
        this._state = parseState(readText(statePath()) ?? '');
        this._displayMode = parseDisplayMode(readText(configPath()) ?? '');
        console.log(`OpenLogi window state: manager enabled (state=${JSON.stringify(this._state)}, ` +
            `mode=${this._displayMode})`);
        this._windowCreatedId = global.display.connect(
            'window-created',
            (_display, window) => {
                console.log('OpenLogi window state: window-created');
                this._track(window);
                // The Wayland client sets app_id/title after the MetaWindow is
                // announced, so a first look sees empty strings. Re-look as soon
                // as the identity lands, then retry after the first commit and
                // once more for slow (software-rendered) starts.
                for (const property of ['notify::wm-class', 'notify::title']) {
                    try {
                        window.connect(property, () => {
                            if (!this._windows.has(window))
                                this._track(window);
                        });
                    } catch (error) {
                        console.log(`OpenLogi window state: no ${property} signal: ${error}`);
                    }
                }
                this._retryTrack(window, 50);
                this._retryTrack(window, 250);
                this._retryTrack(window, 900);
            });
        for (const window of global.display.list_all_windows())
            this._track(window);
        this._watchConfig();
        this._pollTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, POLL_PERIOD_MS, () => {
            this._poll();
            return GLib.SOURCE_CONTINUE;
        });
    }

    disable() {
        this._save();
        if (this._saveTimer) {
            GLib.source_remove(this._saveTimer);
            this._saveTimer = 0;
        }
        for (const timer of this._restoreTimers)
            GLib.source_remove(timer);
        this._restoreTimers.clear();
        for (const [window, record] of this._windows)
            this._untrack(window, record);
        this._windows.clear();
        this._mainTaken = false;
        this._ready.clear();
        this._lastFrame.clear();
        if (this._pollTimer) {
            GLib.source_remove(this._pollTimer);
            this._pollTimer = 0;
        }
        if (this._windowCreatedId) {
            global.display.disconnect(this._windowCreatedId);
            this._windowCreatedId = 0;
        }
        if (this._configMonitor) {
            if (this._configChangedId)
                this._configMonitor.disconnect(this._configChangedId);
            this._configMonitor.cancel();
            this._configMonitor = null;
            this._configChangedId = 0;
        }
        this._state = null;
    }

    _retryTrack(window, delay) {
        const timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, delay, () => {
            this._restoreTimers.delete(timer);
            if (this._windows.has(window))
                return GLib.SOURCE_REMOVE;
            try {
                this._track(window);
            } catch (error) {
                console.log(`OpenLogi window state: retry lookup failed: ${error}`);
            }
            return GLib.SOURCE_REMOVE;
        });
        this._restoreTimers.add(timer);
    }

    _track(window) {
        if (this._windows.has(window))
            return;
        const wmClass = window.get_wm_class();
        const title = window.get_title();
        const kind = windowKind(wmClass, this._mainTaken);
        console.log(`OpenLogi window state: saw ${wmClass ?? '(no wm_class)'} / ` +
            `${title ?? '(no title)'} -> ${kind ?? 'ignored'}`);
        if (TRANSIENT_APP_IDS.has(wmClass))
            return;
        if (!kind)
            return;
        if (kind === 'main')
            this._mainTaken = true;
        const record = {
            kind,
            ids: [
                window.connect('position-changed', () => this._onChanged(window)),
                window.connect('size-changed', () => this._onChanged(window)),
                window.connect('unmanaged', () => this._onUnmanaged(window)),
            ],
        };
        try {
            record.ids.push(window.connect('notify::mapped', () => {
                if (window.mapped && !this._ready.has(window))
                    this._restore(window, kind);
            }));
        } catch (error) {
            console.log(`OpenLogi window state: no mapped signal for ${kind}: ${error}`);
        }
        this._windows.set(window, record);
        this._applyAbove(window, kind);
        // Identity is the earliest moment the saved frame is known. Keep the
        // default compositor frame invisible until the window maps and the
        // remembered frame has been applied; an unmapped move is a no-op.
        if (this._target(window, kind)) {
            const actor = window.get_compositor_private();
            if (actor)
                actor.opacity = 0;
        }
        this._restore(window, kind);
        this._scheduleRestore(window, kind);
    }

    _target(window, kind) {
        const saved = this._state?.[kind];
        if (saved)
            return saved;
        if (kind !== 'flow')
            return null;
        // No extension state yet: adopt the app's own saved position so the
        // first enable does not visibly move a card the user already placed.
        return parseFlowPosition(readText(flowPositionPath()) ?? '');
    }

    _untrack(window, record) {
        for (const id of record.ids) {
            try {
                window.disconnect(id);
            } catch {
                // The window went away with the signal already dispatching.
            }
        }
    }

    _scheduleRestore(window, kind) {
        const timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, RESTORE_DELAY_MS, () => {
            this._restoreTimers.delete(timer);
            this._restore(window, kind);
            return GLib.SOURCE_REMOVE;
        });
        this._restoreTimers.add(timer);
        // A compositor drops a move issued before the surface maps, and the
        // GPUI windows under llvmpipe can take longer than the first delay to
        // map. Retry once after the map if the first attempt did not stick.
        const fallback = GLib.timeout_add(GLib.PRIORITY_DEFAULT, RESTORE_FALLBACK_MS, () => {
            this._restoreTimers.delete(fallback);
            if (!this._ready.has(window))
                this._restore(window, kind);
            return GLib.SOURCE_REMOVE;
        });
        this._restoreTimers.add(fallback);
    }

    _restore(window, kind) {
        if (!this._windows.has(window))
            return;
        if (this._ready.has(window))
            return;
        const target = this._target(window, kind);
        if (!target) {
            this._ready.add(window);
            return;
        }
        // Mutter ignores a move on a Wayland surface that has not mapped yet.
        // The actor stays hidden; the mapped signal calls this again.
        if (window.mapped === false)
            return;
        const actor = window.get_compositor_private();
        if (actor)
            actor.opacity = 0;
        let moved;
        if (kind === 'main') {
            moved = window.move_resize_frame(
                true, target.x, target.y, target.width, target.height);
        } else {
            moved = window.move_frame(true, target.x, target.y);
        }
        if (actor) {
            const timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 80, () => {
                this._restoreTimers.delete(timer);
                try {
                    actor.opacity = 255;
                } catch {
                    // The window went away with the reveal pending.
                }
                return GLib.SOURCE_REMOVE;
            });
            this._restoreTimers.add(timer);
        }
        let frame = {};
        try {
            frame = window.get_frame_rect();
        } catch {
            // The window can disappear between the move and this log.
        }
        console.log(`OpenLogi window state: restore ${kind} saved=${JSON.stringify(target)} ` +
            `moved=${moved} frame=${frame.x},${frame.y} ${frame.width}x${frame.height}`);
        this._ready.add(window);
    }

    _onChanged(window) {
        if (!this._ready.has(window))
            return;
        const record = this._windows.get(window);
        if (!record)
            return;
        try {
            const frame = window.get_frame_rect();
            this._lastFrame.set(window, {
                x: frame.x,
                y: frame.y,
                width: frame.width,
                height: frame.height,
            });
            this._remember(window, record.kind);
        } catch {
            return; // The frame disappeared between the signal and the read.
        }
        this._scheduleSave();
    }

    /// The compositor does not always emit `position-changed` for a Wayland
    /// toplevel moved by the user, so the state is also reconciled once a
    /// second; a frame that moved is saved even when the signal stayed quiet.
    _poll() {
        let changed = false;
        for (const [window, record] of this._windows) {
            if (!this._ready.has(window))
                continue;
            let frame;
            try {
                frame = window.get_frame_rect();
            } catch {
                continue;
            }
            const last = this._lastFrame.get(window);
            if (last && last.x === frame.x && last.y === frame.y &&
                last.width === frame.width && last.height === frame.height)
                continue;
            this._lastFrame.set(window, {
                x: frame.x,
                y: frame.y,
                width: frame.width,
                height: frame.height,
            });
            this._remember(window, record.kind);
            console.log(`OpenLogi window state: moved ${record.kind} -> ` +
                `${frame.x},${frame.y} ${frame.width}x${frame.height}`);
            changed = true;
        }
        if (changed)
            this._save();
    }

    _onUnmanaged(window) {
        const record = this._windows.get(window);
        if (!record)
            return;
        try {
            this._remember(window, record.kind);
        } catch {
            // Keep the last remembered geometry.
        }
        this._untrack(window, record);
        this._windows.delete(window);
        this._ready.delete(window);
        this._lastFrame.delete(window);
        if (record.kind === 'main')
            this._mainTaken = false;
        this._save();
    }

    _remember(window, kind) {
        const frame = window.get_frame_rect();
        this._state ??= {};
        if (kind === 'main') {
            if (frame.width > 0 && frame.height > 0)
                this._state.main = {
                    x: frame.x,
                    y: frame.y,
                    width: frame.width,
                    height: frame.height,
                };
        } else if (kind === 'flow') {
            this._state.flow = {x: frame.x, y: frame.y};
        }
    }

    _scheduleSave() {
        if (this._saveTimer)
            return;
        this._saveTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, SAVE_DELAY_MS, () => {
            this._saveTimer = 0;
            this._save();
            return GLib.SOURCE_REMOVE;
        });
    }

    _save() {
        if (!this._state)
            return;
        writeText(statePath(), serializeState(this._state));
    }

    _applyAbove(window, kind) {
        if (kind !== 'flow')
            return;
        try {
            if (this._displayMode === 'topmost')
                window.make_above();
            else
                window.unmake_above();
        } catch (error) {
            console.log(`OpenLogi window state: could not apply the card mode: ${error}`);
        }
    }

    _watchConfig() {
        try {
            this._configMonitor = Gio.File.new_for_path(configPath()).monitor_file(
                Gio.FileMonitorFlags.NONE,
                null);
            this._configChangedId = this._configMonitor.connect('changed', () => {
                this._displayMode = parseDisplayMode(readText(configPath()) ?? '');
                for (const [window, record] of this._windows)
                    this._applyAbove(window, record.kind);
            });
        } catch (error) {
            console.log(`OpenLogi window state: could not watch config.toml: ${error}`);
        }
    }
}
