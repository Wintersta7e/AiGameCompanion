import assert from 'node:assert/strict';
import { test } from 'node:test';
import { chatViewAction, type ChatView, type ChatViewAction } from './chat.ts';

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
