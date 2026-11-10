// Pure window-state policy for the GNOME extension: which OpenLogi window
// takes part, and how its saved geometry is encoded. Kept free of GI imports
// so Node can exercise it without a shell.

export const MAIN_APP_ID = 'org.openlogi.openlogi';

/// Transient OpenLogi pop-ups that should stay out of GNOME's attention
/// notifications and task lists (and out of persistent window tracking).
export const TRANSIENT_APP_IDS = new Set([
    'openlogi-lock-key-osd',
    'openlogi-notice',
    'openlogi-action-ring',
]);

/// Whether GNOME's "window is ready" attention notification should be
/// suppressed for `wmClass` (transient OSD/notice/ring surfaces).
export function shouldSuppressAttention(wmClass) {
    return TRANSIENT_APP_IDS.has(wmClass);
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

/// 'main' for the first OpenLogi settings window, null for every other window.
///
/// The main window is the first `org.openlogi.openlogi` window. Its OS title is
/// not reliable: on Linux the app draws a client-side titlebar and passes no OS
/// title, so the settings sub-windows share the same empty title and app id.
/// Once the main is taken, later app-id windows are left alone.
export function windowKind(wmClass, mainTaken = false) {
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
    const main = rectangle(raw.main);
    return main ? {main} : null;
}

export function serializeState(state) {
    return `${JSON.stringify(state, null, 2)}\n`;
}
