import assert from 'node:assert/strict';
import { test } from 'node:test';
import { unsavedPick } from './pick.ts';

type Provider = 'gemini' | 'claude' | 'openai';

void test('unsavedPick: a pick the launcher could not save holds when the stored one is read again', () => {
  const pick = unsavedPick<Provider>();
  pick.picked('claude');
  // A read-only run kept the old value; a provider change re-reads it.
  assert.equal(pick.after('gemini'), 'claude');
  assert.equal(pick.after(null), 'claude');
  pick.picked('openai');
  assert.equal(pick.after('gemini'), 'openai');
});

void test('unsavedPick: a saved pick gives way to the stored provider', () => {
  const pick = unsavedPick<Provider>();
  assert.equal(pick.after('gemini'), 'gemini');
  assert.equal(pick.after(null), null);
  pick.picked('claude');
  pick.saved();
  // Another window picked and saved since.
  assert.equal(pick.after('openai'), 'openai');
});
