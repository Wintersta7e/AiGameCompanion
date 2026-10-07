import assert from 'node:assert/strict';
import { test } from 'node:test';
import { FULL_ANSWER_TURN, showHintChips, type ChipMsg } from './hints.ts';

const question: ChipMsg = { role: 'user', content: 'How?' };
const answer: ChipMsg = { role: 'assistant', content: 'Look up.', complete: true, hinted: true };

/** Each case: the messages and flags, and whether the chips show. */
const CASES: [string, ChipMsg[], boolean, boolean, boolean, boolean][] = [
  ['a finished hinted answer', [question, answer], true, false, true, true],
  [
    'an answer that did not finish',
    [question, { role: 'assistant', content: 'Look', hinted: true }],
    true,
    false,
    true,
    false,
  ],
  [
    'an answer asked with hints off',
    [question, { ...answer, hinted: false }],
    true,
    false,
    true,
    false,
  ],
  [
    'an answer with no hint flag, as a loaded one',
    [question, { role: 'assistant', content: 'Look up.', complete: true }],
    true,
    false,
    true,
    false,
  ],
  ['hints first switched off', [question, answer], false, false, true, false],
  ['a request running', [question, answer], true, true, true, false],
  ['nothing can be sent', [question, answer], true, false, false, false],
  [
    'the answer to the full-answer question',
    [{ role: 'user', content: FULL_ANSWER_TURN }, answer],
    true,
    false,
    true,
    false,
  ],
  ['a question as the last message', [question, answer, question], true, false, true, false],
  [
    'an answer still streaming',
    [question, { role: 'assistant', content: 'Lo', streaming: true, hinted: true }],
    true,
    false,
    true,
    false,
  ],
  ['no messages', [], true, false, true, false],
];

void test('showHintChips: shown only after a finished hinted answer', (t) => {
  for (const [name, messages, hintsFirst, asking, canSend, expected] of CASES) {
    assert.equal(showHintChips(messages, hintsFirst, asking, canSend), expected, name);
  }
  t.diagnostic(`fixtures ${String(CASES.length)}`);
});
