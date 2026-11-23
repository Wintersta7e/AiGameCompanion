import assert from 'node:assert/strict';
import { test } from 'node:test';
import { answerCommands, mountComponent, settle, window } from '../test-support/mount.ts';

const availability = {
  gemini: false,
  claude: false,
  openai: false,
  claude_where: '',
  openai_where: '',
  gemini_model: 'm',
  gemini_fallback_model: 'f',
  claude_model: '',
  openai_model: '',
};

// The key store's replies, each held until the test settles it.
const keySaves: { resolve: (value: unknown) => void; reject: (reason: unknown) => void }[] = [];
answerCommands((cmd) => {
  switch (cmd) {
    case 'get_settings':
      return {
        scan_on_startup: true,
        minimize_to_tray: true,
        launch_on_startup: false,
        active_provider: 'gemini',
        gemini_model: '',
        save_chats: true,
      };
    case 'available_providers':
      return availability;
    case 'hotkey_status':
      return [];
    case 'set_gemini_key':
      return new Promise((resolve, reject) => {
        keySaves.push({ resolve, reject });
      });
    default:
      return Promise.reject(new Error(`no mock for ${cmd}`));
  }
});

const url = new URL('./SettingsModal.svelte', import.meta.url);

// A mouse press on a close control: the focused key field blurs first, which
// starts the key save, then the click lands.
async function typeKeyThenClick(target: HTMLElement, control: string) {
  const input = target.querySelector<HTMLInputElement>('input[type="password"]');
  assert.ok(input, 'no key field');
  input.value = 'invented-key';
  input.dispatchEvent(new window.Event('input', { bubbles: true }) as unknown as Event);
  input.dispatchEvent(new window.FocusEvent('blur') as unknown as Event);
  const button =
    target.querySelector<HTMLButtonElement>(`[aria-label="${control}"]`) ??
    [...target.querySelectorAll('button')].find((b) => b.textContent.trim() === control);
  assert.ok(button, `no ${control} button`);
  button.dispatchEvent(new window.MouseEvent('click', { bubbles: true }) as unknown as Event);
  await settle();
}

const isOpen = (target: HTMLElement) => target.querySelector('[role="dialog"]') !== null;

void test('SettingsModal: closing while the key saves waits for the save, then closes', async () => {
  const modal = await mountComponent(url, { open: true });
  await typeKeyThenClick(modal.target, 'Close settings');
  const keySave = keySaves.at(-1);
  assert.equal(keySaves.length, 1, 'the blur did not start one key save');
  assert.equal(isOpen(modal.target), true, 'closed before the key save answered');

  keySave?.resolve({ ...availability, gemini: true });
  await settle();
  assert.equal(isOpen(modal.target), false);
  await modal.unmount();
});

void test('SettingsModal: a key save that fails keeps the modal open with its error', async () => {
  const modal = await mountComponent(url, { open: true });
  await typeKeyThenClick(modal.target, 'Cancel');
  const keySave = keySaves.at(-1);
  assert.equal(keySaves.length, 2, 'the blur did not start one key save');
  assert.equal(isOpen(modal.target), true, 'closed before the key save answered');

  keySave?.reject('the credential store refused the key');
  await settle();
  assert.equal(isOpen(modal.target), true, 'closed although the key was not saved');
  assert.ok(modal.target.textContent.includes('the credential store refused the key'));
  await modal.unmount();
});
