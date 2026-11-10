import assert from 'node:assert/strict';
import test from 'node:test';

import {
    parseState,
    serializeState,
    windowKind,
} from '../openlogi-frontmost@openlogi.dev/windowStatePolicy.js';

test('the first app-id window is the main, and later ones are ignored', () => {
    assert.equal(windowKind('org.openlogi.openlogi', false), 'main');
    assert.equal(windowKind('org.openlogi.openlogi', true), null);
    assert.equal(windowKind(null, false), null);
    assert.equal(windowKind('openlogi-action-ring', false), null);
    assert.equal(windowKind('openlogi-notice', false), null);
    assert.equal(windowKind('openlogi', false), null);
});

test('state parsing keeps a whole rectangle and drops half-invalid entries', () => {
    assert.deepEqual(
        parseState(JSON.stringify({
            main: {x: 10, y: 20, width: 1280, height: 820},
        })),
        {main: {x: 10, y: 20, width: 1280, height: 820}});
    assert.deepEqual(
        parseState(JSON.stringify({main: {x: 1, y: 2, width: 0, height: 3}})),
        null);
    assert.equal(parseState(JSON.stringify({flow: {x: -4, y: 5.5}})), null);
    assert.equal(parseState('not json'), null);
    assert.equal(parseState(''), null);
});

test('serialized state round-trips', () => {
    const state = {main: {x: 10, y: 20, width: 1280, height: 820}};
    assert.deepEqual(parseState(serializeState(state)), state);
    assert.ok(serializeState(state).endsWith('\n'));
});
