import assert from 'node:assert/strict';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { mockIPC } from '@tauri-apps/api/mocks';
import { emit } from '@tauri-apps/api/event';
import { compileModule } from 'svelte/compiler';

// The store's runes need the Svelte compiler, so it is compiled as the build
// does (`dev: false`) and loaded with Tauri's IPC mocked.
const here = dirname(fileURLToPath(import.meta.url));
const source = stripTypeScriptTypes(readFileSync(join(here, 'games.svelte.ts'), 'utf8'));
const { js } = compileModule(source, { dev: false, filename: 'games.svelte.js' });
const outDir = join(here, '../../../node_modules/.cache/games-store-test');
mkdirSync(outDir, { recursive: true });
const compiled = join(outDir, 'games.svelte.js');
writeFileSync(compiled, js.code);

Object.assign(globalThis, { window: globalThis });
const launches: string[] = [];
let answerLaunch: ((value: string) => void) | undefined;
mockIPC(
  (cmd, args) => {
    if (cmd === 'launch_game') {
      launches.push((args as { gameId: string }).gameId);
      return new Promise<string>((resolve) => {
        answerLaunch = resolve;
      });
    }
    return null;
  },
  { shouldMockEvents: true },
);
const store = (await import(pathToFileURL(compiled).href)) as typeof import('./games.svelte.ts');

void test('a game still launching can be launched again', async () => {
  const first = store.launchGame('g1');
  assert.equal(store.getGameStatus('g1'), 'launching');
  // A double click while the launch call has not returned starts nothing.
  await store.launchGame('g1');
  assert.deepEqual(launches, ['g1']);
  answerLaunch?.('launching');
  await first;

  // The game never appeared (a cancelled pre-launch dialog): Launch again.
  assert.equal(store.getGameStatus('g1'), 'launching');
  const second = store.launchGame('g1');
  assert.deepEqual(launches, ['g1', 'g1'], 'launchGame while launching did not invoke the launch');
  assert.equal(store.launchBlocked('g1'), false, 'the Launch button is disabled');
  answerLaunch?.('launching');
  await second;

  // A running game is not launched twice.
  await emit('game-linked', 'g1');
  assert.equal(store.getGameStatus('g1'), 'linked');
  assert.equal(store.launchBlocked('g1'), true);
  await store.launchGame('g1');
  assert.equal(launches.length, 2);
});
