import assert from 'node:assert/strict';
import test from 'node:test';

import {
    parseDisplayMode,
    parseFlowPosition,
    parseState,
    serializeState,
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
