// Mounts an app component in happy-dom for a `node --test` file, compiled as
// the client build compiles it (`dev: false`), with Tauri's IPC mocked: each
// command goes to the handler the test sets with `answerCommands`, and events
// sent with `emitEvent` reach the component's `listen` handlers.
import { register } from 'node:module';
import type { Component } from 'svelte';

register('./svelte-hooks.ts', import.meta.url);

const { Window } = await import('happy-dom');
export const window = new Window({ url: 'http://localhost/' });

// The DOM globals the compiled components and Svelte's runtime read. Node's
// own `Event` family is replaced so events a test dispatches are happy-dom's.
const scope = globalThis as unknown as Record<string, unknown>;
const dom = window as unknown as Record<string, unknown>;
const replaced = ['Event', 'EventTarget', 'CustomEvent', 'navigator'];
for (const name of Object.getOwnPropertyNames(window)) {
  if (name in scope && !replaced.includes(name)) continue;
  try {
    scope[name] = dom[name];
  } catch {
    /* a read-only global Node keeps */
  }
}
scope['window'] = window;
scope['document'] = window.document;

const { mockIPC, mockWindows } = await import('@tauri-apps/api/mocks');
const { emit } = await import('@tauri-apps/api/event');
const svelte = await import('svelte');

/** Answers one IPC command; a returned promise is the command's pending reply. */
export type CommandHandler = (cmd: string, args: Record<string, unknown>) => unknown;

let handler: CommandHandler = (cmd) => Promise.reject(new Error(`no mock for ${cmd}`));
mockIPC((cmd, args) => handler(cmd, (args ?? {}) as Record<string, unknown>), {
  shouldMockEvents: true,
});

export function answerCommands(next: CommandHandler): void {
  handler = next;
}

/** Sends a backend event to the mounted component's listeners. */
export async function emitEvent(event: string, payload: unknown): Promise<void> {
  await emit(event, payload);
  await settle();
}

/** Lets pending replies, timers and Svelte's effects run. */
export async function settle(rounds = 5): Promise<void> {
  svelte.flushSync();
  if (rounds === 0) return;
  await new Promise<void>((done) => {
    setTimeout(done, 0);
  });
  await settle(rounds - 1);
}

export interface Mounted {
  target: HTMLElement;
  unmount: () => Promise<void>;
}

/** Mounts the component at `url` (a `.svelte` file URL) in the window label `label`. */
export async function mountComponent(
  url: URL,
  props: Record<string, unknown>,
  label = 'main',
): Promise<Mounted> {
  mockWindows(label);
  const loaded = (await import(url.href)) as { default: Component<Record<string, unknown>> };
  const target = window.document.createElement('div') as unknown as HTMLElement;
  window.document.body.append(
    target as unknown as Parameters<typeof window.document.body.append>[0],
  );
  const instance = svelte.mount(loaded.default, { target, props });
  await settle();
  return {
    target,
    unmount: async () => {
      await svelte.unmount(instance);
      target.remove();
    },
  };
}
