import assert from 'node:assert/strict';
import { test } from 'node:test';
import { sendsQuestion } from './keys.ts';

const enter = { key: 'Enter', shiftKey: false, isComposing: false, keyCode: 13 };
const letter = { key: 'a', shiftKey: false, isComposing: false, keyCode: 65 };
// Chromium, and so WebView2, reports the Enter that confirms an IME
// composition as `Enter` with `isComposing` set, and some IMEs only as
// keyCode 229.
const confirm = { ...enter, isComposing: true, keyCode: 229 };
const confirmComposing = { ...enter, isComposing: true };
const confirm229 = { ...enter, keyCode: 229 };

void test('sendsQuestion: Enter sends, Shift+Enter does not', () => {
  assert.equal(sendsQuestion(enter), true);
  assert.equal(sendsQuestion({ ...enter, shiftKey: true }), false);
  assert.equal(sendsQuestion(letter), false);
});

void test('sendsQuestion: the Enter that confirms an IME composition does not send', () => {
  assert.equal(sendsQuestion(confirm), false);
  assert.equal(sendsQuestion(confirmComposing), false);
  assert.equal(sendsQuestion(confirm229), false);
});
