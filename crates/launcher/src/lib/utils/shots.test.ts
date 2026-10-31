import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  NO_SHOT,
  afterSend,
  drawFailed,
  drawn,
  heldShot,
  received,
  replaced,
  sendBlocked,
  sendableShot,
  takeFailed,
  taking,
  type ShotHeld,
  type ShotInfo,
  type ShotState,
} from './shots.ts';

const info: ShotInfo = { id: 7, slot: 'chat', width: 640, height: 360, region: false };
const fetched = received(taking(1), info, 1);
const shown = drawn(fetched, info.id);
const failed = takeFailed(taking(1), 1, 'timed out');

/** One state of each kind, as the page reaches it. */
const EVERY_STATE: [string, ShotState][] = [
  ['none', NO_SHOT],
  ['taking', taking(1)],
  ['fetched', fetched],
  ['shown', shown],
  ['failed', failed],
  ['sent', afterSend(shown)],
];

void test('shot states: one of each kind is reachable', () => {
  assert.deepEqual(
    EVERY_STATE.map(([, state]) => state.kind),
    EVERY_STATE.map(([kind]) => kind),
  );
});

void test('sendableShot: an id only once the preview is shown', () => {
  for (const [kind, state] of EVERY_STATE) {
    assert.equal(sendableShot(state), kind === 'shown' ? info.id : null, kind);
  }
});

void test('drawn: a stale id changes nothing', () => {
  assert.equal(drawn(fetched, info.id + 1), fetched);
  for (const [kind, state] of EVERY_STATE) {
    if (kind !== 'fetched') assert.equal(drawn(state, info.id), state, kind);
  }
});

void test('sendBlocked: a take or a fetch in progress blocks a send', () => {
  for (const [kind, state] of EVERY_STATE) {
    assert.equal(sendBlocked(state), kind === 'taking' || kind === 'fetched', kind);
  }
});

void test('takeFailed and drawFailed: a failure blocks nothing and sends no id', () => {
  const failedDraw = drawFailed(fetched, info.id, 'decode failed');
  for (const state of [failed, failedDraw]) {
    assert.equal(state.kind, 'failed');
    assert.equal(sendBlocked(state), false);
    assert.equal(sendableShot(state), null);
  }
  assert.deepEqual(failed, { kind: 'failed', message: 'timed out' });
  const newer = taking(2);
  assert.equal(takeFailed(newer, 1, 'late'), newer, 'an older take fails silently');
  assert.equal(drawFailed(fetched, info.id + 1, 'late'), fetched, 'a stale draw fails silently');
});

void test('received: a stale seq or a slot not taking returns the same object', () => {
  const newer = taking(2);
  assert.equal(received(newer, info, 1), newer);
  for (const [kind, state] of EVERY_STATE) {
    if (kind !== 'taking') assert.equal(received(state, info, 1), state, kind);
  }
  assert.deepEqual(received(taking(3), info, 3), { kind: 'fetched', info });
});

void test('NO_SHOT: a reset leaves nothing to send or wait for', () => {
  assert.deepEqual(NO_SHOT, { kind: 'none' });
  assert.equal(sendableShot(NO_SHOT), null);
  assert.equal(sendBlocked(NO_SHOT), false);
});

void test('afterSend: a shown shot becomes sent, anything else stays', () => {
  assert.deepEqual(afterSend(shown), { kind: 'sent', info });
  for (const [kind, state] of EVERY_STATE) {
    if (kind !== 'shown') assert.equal(afterSend(state), state, kind);
  }
});

void test('heldShot: a hotkey take reported by shot-held lands like a button take', () => {
  const hotkey = { seq: 4, take: 1 };
  const held: ShotHeld = { slot: 'chat', take: 1, shot: info, error: null };
  assert.deepEqual(heldShot(taking(4), held, hotkey), { kind: 'fetched', info });
  const failure: ShotHeld = { slot: 'chat', take: 1, shot: null, error: 'timed out' };
  assert.deepEqual(heldShot(taking(4), failure, hotkey), {
    kind: 'failed',
    message: 'timed out',
  });
});

void test('heldShot: a report after a newer take is stale and changes nothing', () => {
  // The hotkey took seq 4; a Retake since then is seq 5.
  const hotkey = { seq: 4, take: 1 };
  const retake = taking(5);
  const held: ShotHeld = { slot: 'chat', take: 1, shot: info, error: null };
  assert.equal(heldShot(retake, held, hotkey), retake);
  const failure: ShotHeld = { slot: 'chat', take: 1, shot: null, error: 'timed out' };
  assert.equal(heldShot(retake, failure, hotkey), retake);
  for (const [kind, state] of EVERY_STATE) {
    if (kind !== 'taking') assert.equal(heldShot(state, held, { seq: 1, take: 1 }), state, kind);
  }
});

void test('heldShot: a report of an older hotkey take never lands on a newer one', () => {
  // Hotkey take 1 was shown as seq 4, take 2 is shown as seq 5 and waited for.
  const newer = { seq: 5, take: 2 };
  const state = taking(5);
  const older: ShotHeld = { slot: 'chat', take: 1, shot: info, error: null };
  assert.equal(heldShot(state, older, newer), state);
  const olderFailure: ShotHeld = { slot: 'chat', take: 1, shot: null, error: 'timed out' };
  assert.equal(heldShot(state, olderFailure, newer), state);
});

void test('replaced: a take a newer one replaced leaves the slot empty, with no message', () => {
  const hotkey = { seq: 5, take: 2 };
  const gone: ShotHeld = { slot: 'chat', take: 2, shot: null, error: null };
  assert.equal(heldShot(taking(5), gone, hotkey), NO_SHOT);
  assert.equal(replaced(taking(5), 5), NO_SHOT);
  assert.equal(replaced(taking(6), 5).kind, 'taking', 'a newer take is untouched');
  for (const [kind, state] of EVERY_STATE) {
    if (kind !== 'taking') {
      assert.equal(replaced(state, 1), state, kind);
      assert.equal(heldShot(state, gone, { seq: 1, take: 2 }), state, kind);
    }
  }
});
