import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ownKeyOf } from './overlay-input.ts';

const PROVIDERS = {
  gemini: { label: 'Gemini' },
  claude: { label: 'Claude' },
  openai: { label: 'OpenAI' },
};

void test('ownKeyOf: a name every object inherits is not a provider', () => {
  for (const inherited of ['constructor', '__proto__', 'toString', 'valueOf', 'hasOwnProperty']) {
    assert.equal(ownKeyOf(PROVIDERS, inherited), null, inherited);
  }
});

void test('ownKeyOf: a known provider id is a provider; empty or unknown is none', () => {
  for (const id of ['gemini', 'claude', 'openai']) assert.equal(ownKeyOf(PROVIDERS, id), id);
  assert.equal(ownKeyOf(PROVIDERS, ''), null);
  assert.equal(ownKeyOf(PROVIDERS, null), null);
  assert.equal(ownKeyOf(PROVIDERS, undefined), null);
  assert.equal(ownKeyOf(PROVIDERS, 'bogus'), null);
});
