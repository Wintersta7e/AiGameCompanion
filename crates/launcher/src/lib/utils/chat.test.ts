import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  chatViewAction,
  recapOffer,
  RECAP_OFFER_AFTER_MS,
  saveStatusText,
  type ChatView,
  type ChatViewAction,
  type RecapOfferInput,
  type SaveState,
} from './chat.ts';

const view = (displayGeneration: number): ChatView => ({
  displayGeneration,
  exchanges: [],
  save: { state: 'on' },
});

void test('chatViewAction: a view replaces the chat only when its generation changed', (t) => {
  const rows: [string, number, ChatView | null, ChatViewAction][] = [
    ['no view', 3, null, 'empty'],
    ['a newer generation', 3, view(4), 'replace'],
    ['the same generation', 3, view(3), 'status'],
  ];
  for (const [name, current, shown, expected] of rows) {
    const action = chatViewAction(current, shown);
    t.diagnostic(`${name}: ${action}`);
    assert.equal(action, expected, name);
  }
});

void test('saveStatusText: one line per save state, and none of them deletes', (t) => {
  const rows: [string, SaveState | null, boolean, string][] = [
    ['not linked', { state: 'on' }, false, 'No linked game — nothing saved'],
    ['not linked, no state', null, false, 'No linked game — nothing saved'],
    ['no state yet', null, true, ''],
    ['on', { state: 'on' }, true, 'Saved on this PC'],
    [
      'library unreadable',
      { state: 'read_only', reason: 'library' },
      true,
      "Not saving — the library can't be read; saved chats are untouched",
    ],
    [
      'chat file unreadable',
      { state: 'read_only', reason: 'chat_file' },
      true,
      "Not saving — this game's saved chat can't be read; kept until you quit",
    ],
    ['failed', { state: 'failed' }, true, 'Last answer not saved — retrying with the next one'],
    ['skipped', { state: 'skipped' }, true, 'This answer is too long to save'],
    ['off', { state: 'off' }, true, 'Not saved — kept until you quit'],
  ];
  for (const [name, save, linked, expected] of rows) {
    const text = saveStatusText(save, linked);
    t.diagnostic(`${name}: ${text}`);
    assert.equal(text, expected, name);
    const words = ['delete', 'deleted', 'deleting', 'deletion'];
    if (save?.state === 'read_only') assert.ok(!words.some((w) => text.includes(w)), name);
  }
});

void test('recapOffer: the line after a four-hour break, the chip only when a question can be sent', (t) => {
  const now = Date.parse('2026-01-02T12:00:00Z');
  const input = (changes: Partial<RecapOfferInput>): RecapOfferInput => ({
    exchanges: 2,
    newestAnswerAt: now - RECAP_OFFER_AFTER_MS,
    now,
    canSend: true,
    asking: false,
    ...changes,
  });
  const rows: [string, RecapOfferInput, { line: boolean; chip: boolean }][] = [
    ['exactly 4 h', input({}), { line: true, chip: true }],
    [
      '4 h - 1 ms',
      input({ newestAnswerAt: now - RECAP_OFFER_AFTER_MS + 1 }),
      { line: false, chip: false },
    ],
    ['asking', input({ asking: true }), { line: true, chip: false }],
    ['cannot send', input({ canSend: false }), { line: true, chip: false }],
    ['no exchanges', input({ exchanges: 0 }), { line: false, chip: false }],
    ['no answer time', input({ newestAnswerAt: null }), { line: false, chip: false }],
  ];
  assert.equal(RECAP_OFFER_AFTER_MS, 4 * 60 * 60 * 1000);
  for (const [name, given, expected] of rows) {
    const offer = recapOffer(given);
    t.diagnostic(`${name}: line ${offer.line}, chip ${offer.chip}`);
    assert.deepEqual(offer, expected, name);
  }
});
