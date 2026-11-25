import assert from 'node:assert/strict';
import test from 'node:test';

import {
    attachAttentionFilter,
    detachAttentionFilter,
    parseDisplayMode,
    parseFlowPinned,
    parseFlowPosition,
    parseState,
    serializeState,
    shouldSuppressAttention,
    windowKind,
} from '../openlogi-frontmost@openlogi.dev/windowStatePolicy.js';

test('the first app-id window is the main, and the card is separate', () => {
    assert.equal(windowKind('org.openlogi.openlogi', false), 'main');
    assert.equal(windowKind('org.openlogi.openlogi', true), null);
    assert.equal(windowKind('openlogi-flow-indicator', false), 'flow');
    assert.equal(windowKind('openlogi-flow-indicator', true), 'flow');
    assert.equal(windowKind(null, false), null);
    assert.equal(windowKind('openlogi-action-ring', false), null);
    assert.equal(windowKind('openlogi-notice', false), null);
    assert.equal(windowKind('openlogi', false), null);
});

test('state parsing keeps a whole rectangle and drops half-invalid entries', () => {
    assert.deepEqual(
        parseState(JSON.stringify({
            main: {x: 10, y: 20, width: 1280, height: 820},
            flow: {x: 1570, y: 525},
        })),
        {
            main: {x: 10, y: 20, width: 1280, height: 820},
            flow: {x: 1570, y: 525},
        });
    assert.deepEqual(
        parseState(JSON.stringify({main: {x: 1, y: 2, width: 0, height: 3}, flow: {x: 4}})),
        null);
    assert.deepEqual(parseState(JSON.stringify({flow: {x: -4, y: 5.5}})), {flow: {x: -4, y: 5.5}});
    assert.equal(parseState('not json'), null);
    assert.equal(parseState(''), null);
});

test('serialized state round-trips', () => {
    const state = {main: {x: 10, y: 20, width: 1280, height: 820}, flow: {x: 1570, y: 525}};
    assert.deepEqual(parseState(serializeState(state)), state);
    assert.ok(serializeState(state).endsWith('\n'));
});

test('only an explicit topmost display mode lifts the card', () => {
    assert.equal(parseDisplayMode('flow_indicator_display_mode = "topmost"\n'), 'topmost');
    assert.equal(parseDisplayMode('  flow_indicator_display_mode = "normal"\n'), 'normal');
    assert.equal(parseDisplayMode('[app_settings]\nflow_indicator = true\n'), 'normal');
    assert.equal(parseDisplayMode(''), 'normal');
});

test('the app position file seeds the first flow placement', () => {
    assert.deepEqual(parseFlowPosition('x = 1570.0\ny = 525.0\n'), {x: 1570, y: 525});
    assert.deepEqual(parseFlowPosition('x = -4.5\ny = 0\n'), {x: -4.5, y: 0});
    assert.equal(parseFlowPosition('x = 1570.0\n'), null);
    assert.equal(parseFlowPosition(''), null);
});

test('only an explicitly pinned card hands keyboard focus back', () => {
    assert.equal(parseFlowPinned('x = 1\ny = 2\npinned = true\n'), true);
    assert.equal(parseFlowPinned('pinned = false\n'), false);
    assert.equal(parseFlowPinned('x = 1\ny = 2\n'), false);
    assert.equal(parseFlowPinned(''), false);
});

test('transient surfaces and the Flow Indicator never raise attention notifications', () => {
    for (const appId of [
        'openlogi-flow-indicator',
        'openlogi-lock-key-osd',
        'openlogi-notice',
        'openlogi-action-ring',
    ])
        assert.equal(shouldSuppressAttention(appId), true, appId);

    for (const appId of ['org.openlogi.openlogi', 'org.gnome.TextEditor', null, ''])
        assert.equal(shouldSuppressAttention(appId), false, String(appId));
});

test('attention filter swaps Main.windowAttentionHandler listeners cleanly and restores them', () => {
    const connections = new Map();
    const display = {
        connectObject(...args) {
            const owner = args[args.length - 1];
            const map = new Map();
            for (let i = 0; i < args.length - 1; i += 2)
                map.set(args[i], args[i + 1]);
            connections.set(owner, map);
        },
        disconnectObject(owner) {
            connections.delete(owner);
        },
        emit(signal, window) {
            for (const map of connections.values()) {
                const fn = map.get(signal);
                if (fn)
                    fn(display, window);
            }
        },
    };

    const forwarded = [];
    const handler = {
        _onWindowDemandsAttention(d, window) {
            assert.equal(this, handler);
            forwarded.push({display: d, wmClass: window.get_wm_class()});
        },
    };
    const bound = handler._onWindowDemandsAttention.bind(handler);
    display.connectObject(
        'window-demands-attention', bound,
        'window-marked-urgent', bound,
        handler);

    const token = {};
    assert.equal(attachAttentionFilter(display, handler, token), true);
    assert.equal(connections.has(handler), false);
    assert.equal(connections.has(token), true);

    const win = appId => ({get_wm_class: () => appId});
    display.emit('window-demands-attention', win('openlogi-flow-indicator'));
    display.emit('window-marked-urgent', win('openlogi-notice'));
    display.emit('window-demands-attention', win('openlogi-action-ring'));
    assert.deepEqual(forwarded, []);

    display.emit('window-demands-attention', win('org.gnome.TextEditor'));
    display.emit('window-marked-urgent', win('org.openlogi.openlogi'));
    assert.deepEqual(forwarded, [
        {display, wmClass: 'org.gnome.TextEditor'},
        {display, wmClass: 'org.openlogi.openlogi'},
    ]);

    detachAttentionFilter(display, handler, token);
    assert.equal(connections.has(token), false);
    assert.equal(connections.has(handler), true);

    display.emit('window-demands-attention', win('openlogi-flow-indicator'));
    assert.deepEqual(forwarded, [
        {display, wmClass: 'org.gnome.TextEditor'},
        {display, wmClass: 'org.openlogi.openlogi'},
        {display, wmClass: 'openlogi-flow-indicator'},
    ]);
});
