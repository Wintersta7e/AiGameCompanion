import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { Channel } from '@tauri-apps/api/core';
import {
  answerCommands,
  emitEvent,
  mountComponent,
  settle,
  window,
} from '../test-support/mount.ts';

// The overlay draws each held screenshot on a canvas; happy-dom has neither.
Object.assign(globalThis, {
  createImageBitmap: () => Promise.resolve({ width: 10, height: 10, close: () => undefined }),
});
Object.assign(window.HTMLCanvasElement.prototype, {
  getContext: () => ({ drawImage: () => undefined }),
});

interface Asked {
  question: string;
  /** Send one event of the request on its channel, as the backend does. */
  send: (event: Record<string, unknown>) => void;
  finish: () => void;
}
const asked: Asked[] = [];
// What `load_chat` returns: the chat on screen, as the backend shows it.
let chatView: Record<string, unknown> = {
  displayGeneration: 1,
  exchanges: [],
  save: { state: 'on' },
  notice: null,
};
// Each translation request's shot and its reply, held until the test answers.
const translations: { shotId: number; answer: (text: string) => void }[] = [];

answerCommands((cmd, args) => {
  switch (cmd) {
    case 'get_settings':
      return { active_provider: 'gemini', hints_first: false, save_chats: true };
    case 'available_providers':
      return {
        gemini: true,
        claude: false,
        openai: false,
        gemini_model: 'm',
        claude_model: '',
        openai_model: '',
      };
    case 'load_chat':
      return chatView;
    case 'ask_sage': {
      const channel = args['channel'] as Channel<Record<string, unknown>>;
      // Every event of a request carries the chat generation it was admitted under.
      const head = {
        requestId: args['requestId'],
        conversationId: args['conversationId'],
        displayGeneration: chatView['displayGeneration'],
      };
      const send = (event: Record<string, unknown>) => {
        channel.onmessage({ ...head, ...event });
      };
      asked.push({
        question: String(args['question']),
        send,
        finish: () => {
          send({ kind: 'done' });
        },
      });
      return true;
    }
    case 'shot_png':
      return new ArrayBuffer(4);
    case 'drop_shot':
      return null;
    case 'translate_screen':
      return new Promise((resolve) => {
        translations.push({
          shotId: Number(args['shotId']),
          answer: (text) => {
            resolve({ text, cut_short: null });
          },
        });
      });
    default:
      return Promise.reject(new Error(`no mock for ${cmd}`));
  }
});

const overlay = await mountComponent(new URL('./Overlay.svelte', import.meta.url), {}, 'overlay');
await emitEvent('overlay-status', {
  hwnd: 1,
  pid: 2,
  exe: 'invented.exe',
  title: 'Invented Game',
  name: 'Invented Game',
  linked: true,
});

const input = (): HTMLInputElement => {
  const found = overlay.target.querySelector<HTMLInputElement>('input.text-input');
  assert.ok(found, 'no question field');
  return found;
};
const type = async (text: string) => {
  input().value = text;
  input().dispatchEvent(new window.Event('input', { bubbles: true }) as unknown as Event);
  await settle();
};
const click = async (button: HTMLButtonElement | undefined) => {
  assert.ok(button, 'no such button');
  button.dispatchEvent(new window.MouseEvent('click', { bubbles: true }) as unknown as Event);
  await settle();
};
const chip = (label: string) =>
  [...overlay.target.querySelectorAll<HTMLButtonElement>('button.chip')].find(
    (b) => b.textContent.trim() === label,
  );
const finishAnswer = async () => {
  asked.at(-1)?.finish();
  await settle();
};

void test('Overlay: a chip sends its own text and keeps the typed draft', async () => {
  await type('my half-typed draft');
  await click(chip('Where do I go next?'));
  assert.equal(asked.at(-1)?.question, 'Where do I go next?');
  assert.equal(input().value, 'my half-typed draft', 'the chip cleared the draft');
  await finishAnswer();

  // The draft itself, sent, is cleared.
  input().dispatchEvent(
    new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }) as unknown as Event,
  );
  await settle();
  assert.equal(asked.at(-1)?.question, 'my half-typed draft');
  assert.equal(input().value, '');
  await finishAnswer();
});

void test('Overlay: Quick ask stages its question only into an empty field', async () => {
  await type('second draft');
  await emitEvent('quick-ask', { take: null });
  assert.equal(input().value, 'second draft', 'Quick ask replaced the draft');

  await type('');
  await emitEvent('quick-ask', { take: null });
  assert.equal(input().value, 'What should I do next here?');
});

void test('Overlay: a translation that lands after a newer screenshot is not shown under it', async () => {
  // The translate hotkey's screenshot, as the backend reports it once held.
  const hotkeyShot = async (take: number, id: number) => {
    await emitEvent('translate-request', { take });
    await emitEvent('shot-held', {
      slot: 'translate',
      take,
      shot: { id, slot: 'translate', width: 10, height: 10, region: false },
      error: null,
    });
  };
  const translateButton = () =>
    overlay.target.querySelector<HTMLButtonElement>('.translate-actions button') ?? undefined;
  const shownText = () => overlay.target.querySelector('.translate-text')?.textContent ?? null;

  await hotkeyShot(1, 11);
  await click(translateButton());
  assert.deepEqual(
    translations.map((t) => t.shotId),
    [11],
  );

  // The hotkey takes another screenshot while shot 11 is being translated.
  await hotkeyShot(2, 12);
  translations[0]?.answer('TRANSLATION OF SHOT 11');
  await settle();
  assert.equal(shownText(), null, 'the older translation is shown under the newer screenshot');
  assert.equal(translateButton()?.textContent.trim(), 'Translate');

  // Shot 12's own translation is shown.
  await click(translateButton());
  translations[1]?.answer('TRANSLATION OF SHOT 12');
  await settle();
  assert.equal(translations[1]?.shotId, 12);
  assert.equal(shownText(), 'TRANSLATION OF SHOT 12');
});

void test('Overlay: turning saving on while an answer streams keeps the answer on screen', async () => {
  const sageText = () =>
    [...overlay.target.querySelectorAll('.msg.sage .bubble')].map((b) => b.textContent.trim());
  const fromDisk = {
    savedAt: '2026-11-01T10:00:00Z',
    question: 'An older question',
    answer: 'AN OLDER ANSWER',
    provider: 'gemini',
    model: 'm',
    screenshot: false,
  };
  await click(overlay.target.querySelector<HTMLButtonElement>('button.tab') ?? undefined);
  await type('Which way is the exit?');
  input().dispatchEvent(
    new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }) as unknown as Event,
  );
  await settle();
  const request = asked.at(-1);
  assert.equal(request?.question, 'Which way is the exit?');
  request.send({ kind: 'chunk', text: 'THE STREAMED ' });
  await settle();

  // Saving turned on: the saved file is read, so the chat gets a new generation.
  chatView = { displayGeneration: 2, exchanges: [fromDisk], save: { state: 'on' }, notice: null };
  await emitEvent('saved-chats-changed', null);
  assert.ok(
    sageText().some((text) => text.includes('THE STREAMED')),
    `the streaming answer was dropped: ${JSON.stringify(sageText())}`,
  );

  // The answer ends; the store now holds it beside the exchange read from disk.
  request.send({ kind: 'chunk', text: 'ANSWER' });
  chatView = {
    displayGeneration: 2,
    exchanges: [
      fromDisk,
      { ...fromDisk, question: 'Which way is the exit?', answer: 'THE STREAMED ANSWER' },
    ],
    save: { state: 'on' },
    notice: null,
  };
  request.finish();
  await settle();
  assert.deepEqual(sageText(), ['AN OLDER ANSWER', 'THE STREAMED ANSWER'], 'the reloaded chat');
});
