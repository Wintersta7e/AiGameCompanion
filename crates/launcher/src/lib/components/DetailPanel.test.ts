import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  answerCommands,
  emitEvent,
  mountComponent,
  settle,
  window,
} from '../test-support/mount.ts';

const game = {
  id: 'g1',
  name: 'Invented Game',
  source: 'manual',
  source_id: null,
  exe_name: 'invented.exe',
  exe_path: null,
  install_dir: null,
  cover_art_path: null,
  last_played: null,
  play_time_minutes: 0,
};

const launches: string[] = [];
answerCommands((cmd, args) => {
  switch (cmd) {
    case 'get_games':
      return [game];
    case 'launch_game':
      launches.push(String(args['gameId']));
      return 'launching';
    case 'available_providers':
      return {
        gemini: true,
        claude: false,
        openai: false,
        gemini_model: 'm',
        claude_model: '',
        openai_model: '',
      };
    default:
      return Promise.reject(new Error(`no mock for ${cmd}`));
  }
});

const store = (await import(
  new URL('../stores/games.svelte.ts', import.meta.url).href
)) as typeof import('../stores/games.svelte.ts');
await store.loadGames();
const panel = await mountComponent(new URL('./DetailPanel.svelte', import.meta.url), {});

const launchButton = (): HTMLButtonElement => {
  const found = [...panel.target.querySelectorAll('button')].find((b) =>
    /Launch|Running/u.test(b.textContent),
  );
  assert.ok(found, 'no Launch button');
  return found;
};
const click = async (button: HTMLButtonElement) => {
  button.dispatchEvent(new window.MouseEvent('click', { bubbles: true }) as unknown as Event);
  await settle();
};

void test('DetailPanel: a game still launching can be launched again from its button', async () => {
  assert.equal(launchButton().textContent.trim(), 'Launch');
  await click(launchButton());
  assert.deepEqual(launches, ['g1']);

  // The game never appeared: the button stays usable and says so.
  assert.equal(store.getGameStatus('g1'), 'launching');
  assert.equal(launchButton().disabled, false, 'Launch is disabled while launching');
  assert.equal(launchButton().textContent.trim(), 'Launch again');
  await click(launchButton());
  assert.deepEqual(launches, ['g1', 'g1']);

  // A running game is not launched twice.
  await emitEvent('game-linked', 'g1');
  assert.equal(launchButton().disabled, true);
  assert.equal(launchButton().textContent.trim(), 'Running');
});
