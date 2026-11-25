// Pure window-state policy for the GNOME extension: which OpenLogi windows
// take part, and how their saved geometry and flow-indicator settings are
// encoded. Kept free of GI imports so Node can exercise it without a shell.

export const MAIN_APP_ID = 'org.openlogi.openlogi';
export const FLOW_APP_ID = 'openlogi-flow-indicator';

/// Transient OpenLogi pop-ups that should stay out of GNOME's attention
/// notifications and task lists (and out of persistent window tracking).
export const TRANSIENT_APP_IDS = new Set([
    'openlogi-lock-key-osd',
    'openlogi-notice',
    'openlogi-action-ring',
]);

/// Whether GNOME's "window is ready" attention notification should be
/// suppressed for `wmClass`: covers transient OSD/ring surfaces and background
/// Flow Indicator surfaces recreated during Wayland pin swaps.
export function shouldSuppressAttention(wmClass) {
    return TRANSIENT_APP_IDS.has(wmClass) || wmClass === FLOW_APP_ID;
}

/// Replace `Main.windowAttentionHandler`'s display listeners so suppressed
/// OpenLogi windows never raise GNOME's "is ready" notification banner.
export function attachAttentionFilter(display, handler, token) {
    if (!display || !handler || !token ||
        typeof display.disconnectObject !== 'function' ||
        typeof display.connectObject !== 'function' ||
        typeof handler._onWindowDemandsAttention !== 'function')
        return false;

    display.disconnectObject(handler);
    const onDemandsAttention = (d, window) => {
        if (shouldSuppressAttention(window?.get_wm_class?.()))
            return;
        handler._onWindowDemandsAttention(d, window);
    };
    display.connectObject(
        'window-demands-attention', onDemandsAttention,
        'window-marked-urgent', onDemandsAttention,
        token);
    return true;
}

/// Restore `Main.windowAttentionHandler`'s stock display listeners.
export function detachAttentionFilter(display, handler, token) {
    if (!display || !handler || !token)
        return;
    if (typeof display.disconnectObject === 'function')
        display.disconnectObject(token);
    if (typeof display.connectObject === 'function' &&
        typeof handler._onWindowDemandsAttention === 'function') {
        const bound = handler._onWindowDemandsAttention.bind(handler);
        display.connectObject(
            'window-demands-attention', bound,
            'window-marked-urgent', bound,
            handler);
    }
}

/// 'main', 'flow', or null for every other window.
///
/// The main window is the first `org.openlogi.openlogi` window. Its OS title is
/// not reliable: on Linux the app draws a client-side titlebar and passes no OS
/// title, so the settings sub-windows share the same empty title and app id.
/// Once the main is taken, later app-id windows are left alone.
export function windowKind(wmClass, mainTaken = false) {
    if (wmClass === FLOW_APP_ID)
        return 'flow';
    if (wmClass === MAIN_APP_ID && !mainTaken)
        return 'main';
    return null;
}

const finite = value => typeof value === 'number' && Number.isFinite(value);

function rectangle(value) {
    if (!value || !finite(value.x) || !finite(value.y) ||
        !finite(value.width) || !finite(value.height) ||
        value.width <= 0 || value.height <= 0)
        return null;
    return {x: value.x, y: value.y, width: value.width, height: value.height};
}

function position(value) {
    if (!value || !finite(value.x) || !finite(value.y))
        return null;
    return {x: value.x, y: value.y};
}

/// The saved state, with any half-invalid entry dropped. Null when nothing is
/// usable, so a corrupt file behaves as no state rather than as zeroes.
export function parseState(encoded) {
    let raw;
    try {
        raw = JSON.parse(encoded);
    } catch {
        return null;
    }
    if (!raw || typeof raw !== 'object')
        return null;
    const state = {};
    const main = rectangle(raw.main);
    if (main)
        state.main = main;
    const flow = position(raw.flow);
    if (flow)
        state.flow = flow;
    return Object.keys(state).length ? state : null;
}

export function serializeState(state) {
    return `${JSON.stringify(state, null, 2)}\n`;
}

/// The Flow Indicator's display mode from config.toml. Anything but an explicit
/// "topmost" keeps the app's default (normal), including a missing table.
export function parseDisplayMode(toml) {
    const match = /^\s*flow_indicator_display_mode\s*=\s*"([^"]+)"/m.exec(toml ?? '');
    return match?.[1] === 'topmost' ? 'topmost' : 'normal';
}

/// The app's existing flow-indicator.toml position, used once when the
/// extension has no state of its own yet.
export function parseFlowPosition(toml) {
    const source = toml ?? '';
    const x = /^\s*x\s*=\s*(-?(?:\d+(?:\.\d*)?|\.\d+))\s*$/m.exec(source);
    const y = /^\s*y\s*=\s*(-?(?:\d+(?:\.\d*)?|\.\d+))\s*$/m.exec(source);
    if (!x || !y)
        return null;
    const saved = {x: Number(x[1]), y: Number(y[1])};
    return Number.isFinite(saved.x) && Number.isFinite(saved.y) ? saved : null;
}

/// Whether the card is pinned, from the app's flow-indicator.toml. A file
/// written before pinning existed keeps the card regular.
export function parseFlowPinned(toml) {
    return /^\s*pinned\s*=\s*true\s*$/m.test(toml ?? '');
}
