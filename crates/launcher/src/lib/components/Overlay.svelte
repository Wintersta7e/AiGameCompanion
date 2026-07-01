<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';

  type GameInfo = { hwnd: number; pid: number; exe: string; title: string } | null;

  let log = $state<string[]>([]);
  let game = $state<GameInfo>(null);
  let prompt = $state('');
  let response = $state('');
  let asking = $state(false);
  let responseStatus = $state('Ready');

  function push(msg: string) {
    log = [...log.slice(-30), msg];
  }

  async function captureGame() {
    try {
      push(await invoke<string>('capture_game'));
    } catch (error) {
      push(`capture failed: ${String(error)}`);
    }
  }

  async function askSage() {
    const question = prompt.trim();
    if (!question || asking) return;

    response = '';
    responseStatus = 'Streaming';
    asking = true;
    try {
      await invoke('ask_sage', { prompt: question });
    } catch (error) {
      responseStatus = 'Error';
      response = String(error);
      asking = false;
    }
  }

  onMount(() => {
    // Make THIS window's surface transparent (only the overlay window mounts
    // this component, so it does not affect the launcher window).
    document.documentElement.style.background = 'transparent';
    document.body.style.background = 'transparent';
    push('overlay mounted');

    const listeners = [
      listen<GameInfo>('overlay-status', (e) => {
        game = e.payload;
        push(
          game
            ? `detected: ${game.exe} -- "${game.title}" (pid ${game.pid})`
            : 'no game in foreground',
        );
      }),
      listen<string>('sage-token', (e) => {
        response += e.payload;
      }),
      listen('sage-done', () => {
        responseStatus = 'Complete';
        asking = false;
      }),
      listen<string>('sage-error', (e) => {
        responseStatus = 'Error';
        response = e.payload;
        asking = false;
      }),
    ];
    return () => {
      for (const listener of listeners) listener.then((unlisten) => unlisten());
    };
  });
</script>

<div class="overlay-root">
  <div class="panel">
    <div class="brand">SAGE -- spike</div>
    <div class="detected">
      {#if game}
        <strong>{game.title || game.exe}</strong>
        <span class="dim">pid {game.pid} -- hwnd {game.hwnd}</span>
      {:else}
        <span class="dim">no game detected</span>
      {/if}
    </div>
    <button onclick={captureGame}>Capture game</button>
    <div class="chat-input">
      <textarea bind:value={prompt} placeholder="Ask Sage about the game"></textarea>
      <button onclick={askSage} disabled={asking || !prompt.trim()}>
        {asking ? 'Sending...' : 'Send'}
      </button>
    </div>
    <div class="response-label dim">Response -- {responseStatus}</div>
    <div class="response">{response || 'No response yet.'}</div>
    <div class="log">
      {#each log as line}
        <div class="line">{line}</div>
      {/each}
    </div>
    <div class="hint dim">Ctrl+Alt+G toggles the overlay</div>
  </div>
</div>

<style>
  .overlay-root {
    width: 100vw;
    height: 100vh;
    background: transparent;
    display: flex;
    align-items: flex-start;
    justify-content: flex-start;
    padding: 12px;
    box-sizing: border-box;
    font-family: system-ui, sans-serif;
    color: #e9e9ef;
  }
  .panel {
    width: 100%;
    height: 100%;
    background: rgba(14, 14, 18, 0.82);
    border: 1px solid rgba(224, 162, 60, 0.45);
    border-radius: 14px;
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    box-sizing: border-box;
  }
  .brand {
    color: #e0a23c;
    font-weight: 700;
    letter-spacing: 0.08em;
  }
  .detected {
    display: flex;
    flex-direction: column;
  }
  .dim {
    color: #8a8a97;
    font-size: 12px;
  }
  .log {
    max-height: 120px;
    overflow: auto;
    background: rgba(255, 255, 255, 0.03);
    border-radius: 8px;
    padding: 8px;
    font-size: 12px;
    font-family: ui-monospace, monospace;
  }
  .line {
    white-space: pre-wrap;
  }
  .hint {
    font-size: 11px;
  }
  .chat-input {
    display: flex;
    align-items: flex-end;
    gap: 8px;
  }
  textarea {
    flex: 1;
    min-height: 58px;
    resize: vertical;
    background: rgba(255, 255, 255, 0.06);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 8px;
    color: #fff;
    padding: 8px 10px;
    outline: none;
  }
  .response-label {
    margin-bottom: -4px;
  }
  .response {
    flex: 1;
    min-height: 80px;
    overflow: auto;
    white-space: pre-wrap;
    background: rgba(255, 255, 255, 0.04);
    border-radius: 8px;
    padding: 10px;
  }
  button {
    align-self: flex-start;
    background: #e0a23c;
    border: 0;
    border-radius: 8px;
    color: #16120b;
    cursor: pointer;
    font-weight: 700;
    padding: 7px 12px;
  }
  button:disabled {
    cursor: default;
    opacity: 0.55;
  }
</style>
