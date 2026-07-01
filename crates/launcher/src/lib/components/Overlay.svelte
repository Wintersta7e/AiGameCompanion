<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke, Channel } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';

  type GameInfo = { hwnd: number; pid: number; exe: string; title: string } | null;
  type Provider = 'gemini' | 'claude' | 'openai';
  type Availability = { gemini: boolean; claude: boolean; openai: boolean };
  type ChatTurn = { role: 'user' | 'assistant'; content: string };
  type SageEvent = {
    kind: 'chunk' | 'done' | 'error';
    requestId: number;
    conversationId: number;
    text?: string;
    message?: string;
  };

  const PROVIDER_LABELS: Record<Provider, string> = {
    gemini: 'Gemini',
    claude: 'Claude',
    openai: 'OpenAI',
  };

  let log = $state<string[]>([]);
  let game = $state<GameInfo>(null);
  let prompt = $state('');
  let response = $state('');
  let asking = $state(false);
  let responseStatus = $state('Ready');

  let availability = $state<Availability>({ gemini: false, claude: false, openai: false });
  let provider = $state<Provider>('gemini');
  let attachScreenshot = $state(false);

  // Conversation history sent with each request (backend accepts multi-turn).
  let history = $state<ChatTurn[]>([]);
  // The provider persisted in launcher settings; restored once available.
  let savedProvider: Provider | null = null;

  // Plain counters (not reactive): identify requests and conversations so the UI
  // can ignore streamed output from a superseded request or conversation. Real
  // request ids start at 1, so 0 means "no live request".
  let nextRequestId = 0;
  let conversationId = 1;
  let activeRequestId = 0;

  const available = $derived(
    (['gemini', 'claude', 'openai'] as Provider[]).filter((p) => availability[p]),
  );
  const screenshotSupported = $derived(provider !== 'openai');

  function push(msg: string) {
    log = [...log.slice(-30), msg];
  }

  // Re-query provider availability. The background CLI-detection thread can take
  // seconds (WSL), so a fetch at startup may miss Claude/OpenAI; re-fetching each
  // time the overlay is shown picks them up once detection has completed.
  async function refreshProviders() {
    try {
      availability = await invoke<Availability>('available_providers');
    } catch (error) {
      push(`provider check failed: ${String(error)}`);
      return;
    }
    if (savedProvider && availability[savedProvider]) {
      provider = savedProvider;
    } else if (!availability[provider] && available.length > 0) {
      provider = available[0];
    }
  }

  async function captureGame() {
    try {
      push(await invoke<string>('capture_game'));
    } catch (error) {
      push(`capture failed: ${String(error)}`);
    }
  }

  async function selectProvider(next: Provider) {
    provider = next;
    savedProvider = next;
    try {
      await invoke('set_active_provider', { provider: next });
    } catch (error) {
      push(`failed to save provider: ${String(error)}`);
    }
  }

  async function newChat() {
    // Cancel any in-flight request so its stream cannot bleed into the new chat.
    if (asking) {
      const id = activeRequestId;
      activeRequestId = 0;
      asking = false;
      try {
        await invoke('cancel_sage', { requestId: id });
      } catch (error) {
        push(`cancel failed: ${String(error)}`);
      }
    }
    activeRequestId = 0;
    conversationId += 1;
    history = [];
    response = '';
    responseStatus = 'Ready';
    prompt = '';
  }

  async function askSage() {
    const question = prompt.trim();
    if (!question || asking) return;

    const id = (nextRequestId += 1);
    const convo = conversationId;
    activeRequestId = id;
    const userTurn: ChatTurn = { role: 'user', content: question };
    const outgoing = [...history, userTurn];
    response = '';
    responseStatus = 'Streaming';
    asking = true;

    const channel = new Channel<SageEvent>();
    channel.onmessage = (event) => {
      // Ignore output from a superseded request or a cleared conversation.
      if (event.requestId !== activeRequestId || event.conversationId !== convo) return;
      if (event.kind === 'chunk') {
        response += event.text ?? '';
      } else if (event.kind === 'done') {
        responseStatus = 'Complete';
        asking = false;
        history = [...history, userTurn, { role: 'assistant', content: response }];
      } else if (event.kind === 'error') {
        responseStatus = 'Error';
        const msg = event.message ?? 'Unknown error';
        // Preserve any partial answer instead of discarding it.
        response = response ? `${response}\n\n[error] ${msg}` : msg;
        asking = false;
      }
    };

    try {
      await invoke('ask_sage', {
        requestId: id,
        conversationId: convo,
        provider,
        messages: outgoing,
        attachScreenshot: attachScreenshot && screenshotSupported,
        channel,
      });
      prompt = '';
    } catch (error) {
      responseStatus = 'Error';
      response = String(error);
      asking = false;
    }
  }

  async function stopSage() {
    if (!asking) return;
    const id = activeRequestId;
    // Drop any chunk already dispatched before the backend abort takes effect.
    activeRequestId = 0;
    asking = false;
    responseStatus = 'Stopped';
    try {
      await invoke('cancel_sage', { requestId: id });
    } catch (error) {
      push(`cancel failed: ${String(error)}`);
    }
  }

  onMount(() => {
    // Make THIS window's surface transparent (only the overlay window mounts
    // this component, so it does not affect the launcher window).
    document.documentElement.style.background = 'transparent';
    document.body.style.background = 'transparent';
    push('overlay mounted');

    void (async () => {
      try {
        const settings = await invoke<{ active_provider?: string }>('get_settings');
        savedProvider = (settings.active_provider as Provider | undefined) ?? null;
      } catch (error) {
        push(`settings load failed: ${String(error)}`);
      }
      await refreshProviders();
    })();

    const listeners = [
      listen<GameInfo>('overlay-status', (e) => {
        game = e.payload;
        push(
          game
            ? `detected: ${game.exe} -- "${game.title}" (pid ${game.pid})`
            : 'no game in foreground',
        );
        // The overlay just became visible: CLI detection has had time to finish.
        void refreshProviders();
      }),
    ];
    return () => {
      for (const listener of listeners) listener.then((unlisten) => unlisten());
    };
  });
</script>

<div class="overlay-root">
  <div class="panel">
    <div class="titlebar" data-tauri-drag-region>
      <span class="brand">SAGE</span>
      <span class="drag-hint dim">drag to move</span>
    </div>
    <div class="detected">
      {#if game}
        <strong>{game.title || game.exe}</strong>
        <span class="dim">pid {game.pid} -- hwnd {game.hwnd}</span>
      {:else}
        <span class="dim">no game detected</span>
      {/if}
    </div>

    <div class="controls">
      <select
        aria-label="Provider"
        value={provider}
        onchange={(e) => selectProvider(e.currentTarget.value as Provider)}
        disabled={available.length === 0}
      >
        {#if available.length === 0}
          <option value={provider}>No providers available</option>
        {:else}
          {#each available as p}
            <option value={p}>{PROVIDER_LABELS[p]}</option>
          {/each}
        {/if}
      </select>
      <label class="attach" class:disabled={!screenshotSupported}>
        <input type="checkbox" bind:checked={attachScreenshot} disabled={!screenshotSupported} />
        Attach screenshot
      </label>
      <button class="ghost" onclick={captureGame}>Capture</button>
      <button class="ghost" onclick={newChat}>New chat</button>
    </div>

    <div class="chat-input">
      <textarea bind:value={prompt} placeholder="Ask Sage about the game"></textarea>
      {#if asking}
        <button onclick={stopSage}>Stop</button>
      {:else}
        <button onclick={askSage} disabled={!prompt.trim() || available.length === 0}>Send</button>
      {/if}
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
  .titlebar {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    cursor: move;
    user-select: none;
    /* Stretch the grab area to the panel padding edges. */
    margin: -6px -8px 2px -8px;
    padding: 6px 10px 4px;
  }
  .brand {
    color: #e0a23c;
    font-weight: 700;
    letter-spacing: 0.08em;
  }
  .drag-hint {
    font-size: 10px;
  }
  .detected {
    display: flex;
    flex-direction: column;
  }
  .dim {
    color: #8a8a97;
    font-size: 12px;
  }
  .controls {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    font-size: 12px;
  }
  .controls select {
    background: rgba(255, 255, 255, 0.06);
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 8px;
    color: #fff;
    padding: 5px 8px;
    outline: none;
  }
  .attach {
    display: flex;
    align-items: center;
    gap: 4px;
    cursor: pointer;
  }
  .attach.disabled {
    opacity: 0.5;
    cursor: default;
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
  button.ghost {
    background: rgba(255, 255, 255, 0.08);
    color: #e9e9ef;
    font-weight: 600;
    padding: 5px 10px;
  }
  button:disabled {
    cursor: default;
    opacity: 0.55;
  }
</style>
