// Pure window-state policy for the GNOME extension: which OpenLogi window
// takes part, and how its saved geometry is encoded. Kept free of GI imports
// so Node can exercise it without a shell.

export const MAIN_APP_ID = 'org.openlogi.openlogi';

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
