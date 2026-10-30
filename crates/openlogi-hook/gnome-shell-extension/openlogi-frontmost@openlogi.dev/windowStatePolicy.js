// Pure window-state policy for the GNOME extension: which OpenLogi windows
// take part, and how their saved geometry and flow-indicator settings are
// encoded. Kept free of GI imports so Node can exercise it without a shell.

export const MAIN_APP_ID = 'org.openlogi.openlogi';
export const FLOW_APP_ID = 'openlogi-flow-indicator';

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
