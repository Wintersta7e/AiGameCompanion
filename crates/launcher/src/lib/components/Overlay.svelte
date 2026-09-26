<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { invoke, Channel } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { hashHue } from '../utils/accent';
  import { PROVIDERS, modelName, type ModelNames, type Provider } from '../stores/companion.svelte';

  type GameInfo = {
    hwnd: number;
    pid: number;
    exe: string;
    title: string;
    name: string;
    linked: boolean;
    accent?: string;
  } | null;
  interface Availability extends ModelNames {
    gemini: boolean;
    claude: boolean;
    openai: boolean;
    gemini_fallback_model: string;
  }
  interface SageEvent {
    kind: 'chunk' | 'done' | 'error';
    requestId: number;
    conversationId: number;
    text?: string;
    message?: string;
  }
  interface Msg {
    role: 'user' | 'assistant';
    content: string;
    model?: string;
    screenshot?: boolean;
    streaming?: boolean;
    // Set only when the answer finished ("done"); stopped and failed ones never are.
    complete?: boolean;
  }

  const PROVIDER_ORDER: Provider[] = ['gemini', 'claude', 'openai'];
  const SUGGESTIONS = ['Where do I go next?', "What's this enemy weak to?", 'Explain this screen'];

  let game = $state<GameInfo>(null);
  let availability = $state<Availability>({
    gemini: false,
    claude: false,
    openai: false,
    gemini_model: '',
    gemini_fallback_model: '',
    claude_model: '',
    openai_model: '',
  });
  let provider = $state<Provider>('gemini');
  let savedProvider: Provider | null = null;
  let dropdownOpen = $state(false);
  let tab = $state<'chat' | 'translate'>('chat');
  let attach = $state(false);
  let prompt = $state('');
  let asking = $state(false);
  let messages = $state<Msg[]>([]);

  let inputEl = $state<HTMLInputElement | null>(null);
  let translateBtn = $state<HTMLButtonElement | null>(null);
  let msglistEl = $state<HTMLDivElement | null>(null);

  let translateText = $state('');
  let translateBusy = $state(false);
  let translateError = $state('');

  const QUICK_ASK = 'What should I do next here?';

  // Plain counters (not reactive): real request ids start at 1, so 0 = "none".
  let nextRequestId = 0;
  let conversationId = 1;
  let activeRequestId = 0;
  let streamIndex = -1;
  let savedProviderLoaded = false;

  const available = $derived(PROVIDER_ORDER.filter((p) => availability[p]));
  const meta = $derived(PROVIDERS[provider]);
  // The model each provider answers with, as the backend reports it.
  const modelLabel = (p: Provider) => modelName(availability, p);
  const accent = $derived(
    game ? (game.accent ?? hashHue(game.exe || game.title || 'sage')) : '#e0a23c',
  );
  // Nothing about a window is sent until it is linked; the backend enforces the
  // same gate, this only keeps the controls honest.
  const canAttach = $derived(Boolean(game?.linked));
  const canSend = $derived(Boolean(game?.linked) && availability[provider]);
  // The exe's file name, for the link control ("Link foo.exe ...").
  const exeFile = $derived(game?.exe.split(/[\\/]/).pop() ?? '');
  const captureHint = $derived.by(() => {
    // Name the window Enter will capture: a hotkey pressed while the overlay
    // is open acts on this stored target, not on whatever is in front now.
    if (attach && canAttach && game)
      return `screenshot of ${game.name || game.exe} attaches on send`;
    return 'screenshot attaches via WGC';
  });

  // Follow a streaming answer, but only while the user is already at the bottom
  // -- scrolling up to re-read history must not be yanked back down. The check
  // has to happen before the DOM grows, hence $effect.pre.
  $effect.pre(() => {
    // Re-runs on every appended chunk and on every new message.
    const growth = messages.length + (messages.at(-1)?.content.length ?? 0);
    const el = msglistEl;
    if (!el || growth === 0) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight > 50) return;
    void tick().then(() => {
      el.scrollTop = el.scrollHeight;
    });
  });

  // Re-query availability (CLI detection can lag startup). A saved provider
  // stays selected even while it is unavailable: only the user switches. With
  // nothing saved, start on the first available one.
  async function refreshProviders() {
    try {
      availability = await invoke<Availability>('available_providers');
    } catch {
      return;
    }
    const fallback = available[0];
    if (savedProvider) provider = savedProvider;
    else if (!availability[provider] && fallback) provider = fallback;
  }

  async function selectProvider(p: Provider) {
    provider = p;
    savedProvider = p;
    dropdownOpen = false;
    try {
      await invoke('set_active_provider', { provider: p });
    } catch {
      /* selection still applies for this session */
    }
  }

  async function newChat() {
    const inflight = asking ? activeRequestId : 0;
    // Reset synchronously first so a Send fired during the cancel IPC gap cannot
    // be clobbered by a post-await state reset.
    activeRequestId = 0;
    asking = false;
    conversationId += 1;
    messages = [];
    prompt = '';
    if (inflight) {
      try {
        await invoke('cancel_sage', { requestId: inflight });
      } catch {
        /* best effort */
      }
    }
  }

  function onWindowPointerDown(event: PointerEvent) {
    if (!dropdownOpen) return;
    const target = event.target as HTMLElement;
    if (!target.closest('.provider-pill') && !target.closest('.dropdown')) {
      dropdownOpen = false;
    }
  }

  // The history a request carries: each question with the answer that followed
  // it, only when that answer finished and has text. A stopped, failed or empty
  // answer is left out together with its question, so no error text or half
  // answer is sent back as if the model had said it.
  function completedHistory(msgs: Msg[]): { role: Msg['role']; content: string }[] {
    const history: { role: Msg['role']; content: string }[] = [];
    for (let i = 0; i + 1 < msgs.length; i += 2) {
      const question = msgs[i];
      const answer = msgs[i + 1];
      if (
        question?.role === 'user' &&
        answer?.role === 'assistant' &&
        answer.complete === true &&
        answer.content.trim() !== ''
      ) {
        history.push(
          { role: 'user', content: question.content },
          { role: 'assistant', content: answer.content },
        );
      }
    }
    return history;
  }

  async function send(text?: string) {
    const question = (text ?? prompt).trim();
    if (!question || asking || !canSend) return;

    const id = (nextRequestId += 1);
    const convo = conversationId;
    activeRequestId = id;
    const withShot = attach && canAttach;

    // History for the backend: finished exchanges + this question.
    const outgoing = completedHistory(messages);
    outgoing.push({ role: 'user', content: question });

    messages = [
      ...messages,
      { role: 'user', content: question, screenshot: withShot },
      { role: 'assistant', content: '', model: modelLabel(provider), streaming: true },
    ];
    const idx = messages.length - 1;
    streamIndex = idx;
    prompt = '';
    asking = true;

    const channel = new Channel<SageEvent>();
    channel.onmessage = (event) => {
      // Ignore output from a superseded request or cleared conversation.
      if (event.requestId !== activeRequestId || event.conversationId !== convo) return;
      const bubble = messages[idx];
      if (!bubble) return;
      if (event.kind === 'chunk') {
        bubble.content += event.text ?? '';
      } else if (event.kind === 'done') {
        bubble.streaming = false;
        bubble.complete = true;
        asking = false;
      } else {
        const msg = event.message ?? 'Unknown error';
        bubble.content = bubble.content ? `${bubble.content}\n\n[error] ${msg}` : `[error] ${msg}`;
        bubble.streaming = false;
        asking = false;
      }
    };

    try {
      await invoke('ask_sage', {
        requestId: id,
        conversationId: convo,
        provider,
        messages: outgoing,
        attachScreenshot: withShot,
        channel,
      });
    } catch (err) {
      // Same guard as the channel handler: a rejection that lands after New chat
      // or a newer Send must not write into the current conversation's bubble.
      if (id !== activeRequestId || convo !== conversationId) return;
      const bubble = messages[idx];
      if (bubble) {
        bubble.content = `[error] ${String(err)}`;
        bubble.streaming = false;
      }
      asking = false;
    }
  }

  async function stop() {
    if (!asking) return;
    const id = activeRequestId;
    activeRequestId = 0;
    asking = false;
    const streamed = messages[streamIndex];
    if (streamed) streamed.streaming = false;
    try {
      await invoke('cancel_sage', { requestId: id });
    } catch {
      /* best effort */
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void send();
    }
  }

  async function hideOverlay() {
    // Go through the backend: it hands focus back to the game, which a bare
    // window.hide() skips.
    try {
      await invoke('hide_overlay');
    } catch {
      /* command may not exist in preview */
    }
  }

  async function linkGame() {
    const shown = game;
    if (!shown) return;
    const linked = await invoke<GameInfo>('link_game', { hwnd: shown.hwnd, pid: shown.pid }).catch(
      () => null,
    );
    // An overlay-status may have replaced the target while the call was in
    // flight: the result applies only to the window it was asked for.
    if (!linked || game?.hwnd !== shown.hwnd || game.pid !== shown.pid) return;
    game = linked;
    await tick();
    inputEl?.focus();
  }

  async function runTranslate() {
    if (translateBusy) return;
    if (!availability.gemini) {
      translateText = '';
      translateError = 'Translation requires a Gemini API key.';
      return;
    }
    translateBusy = true;
    translateError = '';
    try {
      const res = await invoke<{ text: string }>('translate_screen');
      translateText = res.text;
    } catch (err) {
      translateError = String(err);
      translateText = '';
    } finally {
      translateBusy = false;
    }
  }

  // The hotkey only stages the question: nothing is captured or sent, and no
  // answer in progress is cancelled, until the user presses Enter or clicks.
  async function runQuickAsk() {
    tab = 'chat';
    prompt = QUICK_ASK;
    if (canAttach) attach = true;
    await tick();
    inputEl?.focus();
  }

  async function copyTranslation() {
    if (!translateText) return;
    try {
      await navigator.clipboard.writeText(translateText);
    } catch {
      /* clipboard may be unavailable */
    }
  }

  // Read the saved provider: at mount, and again whenever a window announces a
  // provider change (a choice made in the launcher, a key saved in Settings,
  // CLI detection finishing).
  async function loadSavedProvider() {
    try {
      const settings = await invoke<{ active_provider?: string }>('get_settings');
      // The saved provider is kept even when unavailable. An empty value (a
      // new install, nothing picked yet) or an unknown one (a hand-edited
      // state file) is no saved provider, so the first available one is used.
      const saved = settings.active_provider;
      savedProvider = saved && saved in PROVIDERS ? (saved as Provider) : null;
    } catch {
      /* defaults apply */
    }
    savedProviderLoaded = true;
    await refreshProviders();
  }

  onMount(() => {
    // Only the overlay window mounts this; keep its surface transparent.
    document.documentElement.style.background = 'transparent';
    document.body.style.background = 'transparent';

    void loadSavedProvider();

    const listeners = [
      listen<GameInfo>('overlay-status', (event) => {
        game = event.payload;
        // The overlay just became visible: CLI detection has had time to finish.
        if (savedProviderLoaded) void refreshProviders();
        // The window is shown and hidden, never remounted, so the input has to
        // be focused on every show -- the Rust side only focuses the window.
        // Wait for the DOM: `game` above flips canSend, and a still-disabled
        // input silently refuses focus.
        void tick().then(() => inputEl?.focus());
      }),
      listen('providers-changed', () => {
        void loadSavedProvider();
      }),
      listen('translate-request', () => {
        // Stage only: Enter on the focused button, or a click, runs it.
        tab = 'translate';
        void tick().then(() => translateBtn?.focus());
      }),
      listen('quick-ask', () => {
        void runQuickAsk();
      }),
    ];
    return () => {
      for (const listener of listeners)
        void listener.then((unlisten) => {
          unlisten();
        });
    };
  });
</script>

<svelte:window onpointerdown={onWindowPointerDown} />

<div style="--accent: {accent};" class="overlay-root">
  <div class="panel">
    <!-- titlebar -->
    <div class="titlebar" data-tauri-drag-region>
      <span class="logo"></span>
      <span class="wordmark">SAGE</span>
      <span class="drag-chip">drag</span>
      <div class="title-actions">
        <button
          class="icon-btn"
          aria-label="New chat"
          onclick={newChat}
          title="New chat"
          type="button"
        >
          <svg
            fill="none"
            height="15"
            stroke="currentColor"
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="1.7"
            viewBox="0 0 24 24"
            width="15"><path d="M3 12a9 9 0 1 0 3-6.7L3 8" /><path d="M3 3v5h5" /></svg
          >
        </button>
        <button
          class="icon-btn"
          aria-label="Hide"
          onclick={hideOverlay}
          title="Hide (Ctrl+Shift+G)"
          type="button"
        >
          <svg
            fill="none"
            height="15"
            stroke="currentColor"
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="1.9"
            viewBox="0 0 24 24"
            width="15"><path d="M6 9l6 6 6-6" /></svg
          >
        </button>
      </div>
    </div>

    <!-- detected game -->
    <div class="gamebar">
      <span class="game-tile" class:muted={!game}></span>
      <div class="game-meta">
        {#if game?.exe}
          <span class="game-title">{game.name || game.exe}</span>
          <span class="game-exe">{game.exe}</span>
        {:else if game}
          <span class="game-title dim">This window cannot be identified</span>
          <span class="game-exe">nothing about it is sent</span>
        {:else}
          <span class="game-title dim">No game detected</span>
          <span class="game-exe">bring a game to the foreground</span>
        {/if}
      </div>
      {#if game?.linked}
        <span class="linked-pill"><span class="d"></span>linked</span>
      {:else if game?.exe}
        <button class="link-btn" onclick={linkGame} title={game.exe} type="button"
          >Link {exeFile} for this session</button
        >
      {/if}
    </div>

    <!-- tabs + provider -->
    <div class="tabrow">
      <div class="tabs">
        <button
          class="tab"
          class:active={tab === 'chat'}
          onclick={() => (tab = 'chat')}
          type="button"
        >
          <svg
            fill="none"
            height="14"
            stroke="currentColor"
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="1.7"
            viewBox="0 0 24 24"
            width="14"
            ><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" /></svg
          >
          Chat
        </button>
        <button
          class="tab"
          class:active={tab === 'translate'}
          onclick={() => (tab = 'translate')}
          type="button"
        >
          <svg
            fill="none"
            height="14"
            stroke="currentColor"
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="1.7"
            viewBox="0 0 24 24"
            width="14"
            ><circle cx="12" cy="12" r="9" /><path
              d="M3 12h18M12 3a15 15 0 0 1 0 18M12 3a15 15 0 0 0 0 18"
            /></svg
          >
          Translate
        </button>
      </div>
      {#if tab === 'chat'}
        <button
          class="provider-pill"
          disabled={available.length === 0 || asking}
          onclick={() => (dropdownOpen = !dropdownOpen)}
          type="button"
        >
          <span style="background: {meta.dot}; box-shadow: 0 0 6px {meta.dot};" class="prov-dot"
          ></span>
          {availability[provider] ? meta.label : `${meta.label} — not available`}
          <span class="caret">{dropdownOpen ? '▴' : '▾'}</span>
        </button>

        {#if dropdownOpen && available.length > 0}
          <div class="dropdown">
            <div class="dropdown-head">Available providers</div>
            {#each available as p (p)}
              <button class="prov-row" onclick={() => selectProvider(p)} type="button">
                <span style="background: {PROVIDERS[p].dot};" class="pdot"></span>
                <span class="pmeta">
                  <span class="pname">{PROVIDERS[p].label}</span>
                  <span class="pmodel">{modelLabel(p)}</span>
                </span>
                {#if p === provider}
                  <span class="pcheck">
                    <svg
                      fill="none"
                      height="15"
                      stroke="currentColor"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                      stroke-width="2.2"
                      viewBox="0 0 24 24"
                      width="15"><path d="M5 13l4 4L19 7" /></svg
                    >
                  </span>
                {/if}
              </button>
            {/each}
          </div>
        {/if}
      {:else}
        <!-- translation always goes to Gemini, whichever chat provider is picked -->
        <span class="provider-note">
          <span
            style="background: {PROVIDERS.gemini.dot}; box-shadow: 0 0 6px {PROVIDERS.gemini.dot};"
            class="prov-dot"
          ></span>
          Translates with Gemini
        </span>
      {/if}
    </div>

    {#if tab === 'chat'}
      <!-- chat body -->
      <div class="body">
        <div bind:this={msglistEl} class="msglist">
          {#if available.length === 0}
            <div class="msg sage">
              <span class="avatar"></span>
              <div class="bubble intro">
                No AI providers are available. Add a Gemini key in Settings, or install and sign in
                to the Claude or Codex CLI.
              </div>
            </div>
          {:else if messages.length === 0}
            <div class="msg sage">
              <span class="avatar"></span>
              <div class="bubble intro">
                {#if game?.linked}
                  Linked to {game.name || game.exe}. Ask me anything, or tap a prompt below — turn
                  on the image button to include a screenshot.
                {:else if game?.exe}
                  Nothing about this window is sent until you link it. Link {exeFile} above to ask about
                  it this session.
                {:else if game}
                  This window cannot be identified, so it cannot be linked and nothing about it is
                  sent.
                {:else}
                  Bring a game to the foreground and I'll link to it. Then ask me anything about
                  what's on screen.
                {/if}
              </div>
            </div>
            {#if game?.linked}
              <div class="chips">
                {#each SUGGESTIONS as s (s)}
                  <button class="chip" onclick={() => send(s)} type="button">{s}</button>
                {/each}
              </div>
            {/if}
          {:else}
            {#each messages as m, i (i)}
              {#if m.role === 'user'}
                <div class="msg user">
                  {#if m.screenshot}
                    <span class="frame-chip"><span class="thumb"></span>frame · WGC</span>
                  {/if}
                  <div class="bubble">{m.content}</div>
                </div>
              {:else}
                <div class="msg sage">
                  <span class="avatar"></span>
                  <div>
                    <div class="bubble">
                      {#if m.content}{m.content}{/if}{#if m.streaming && m.content}<span
                          class="caret-blink"
                        ></span>{/if}
                      {#if m.streaming && !m.content}
                        <span class="thinking"><i></i><i></i><i></i></span>
                      {/if}
                    </div>
                    {#if m.model && (m.content || !m.streaming)}
                      <div class="meta">{m.model}{m.streaming ? ' · streaming' : ''}</div>
                    {/if}
                  </div>
                </div>
              {/if}
            {/each}
          {/if}
        </div>

        <div class="inputbar">
          <div class="inputrow">
            <button
              class="attach-btn"
              class:off={!(attach && canAttach)}
              aria-label="Attach screenshot"
              disabled={!canAttach}
              onclick={() => (attach = !attach)}
              title="Attach a screenshot of the game"
              type="button"
            >
              <svg
                fill="none"
                height="18"
                stroke="currentColor"
                stroke-linecap="round"
                stroke-linejoin="round"
                stroke-width="1.7"
                viewBox="0 0 24 24"
                width="18"
                ><rect height="18" rx="2" width="18" x="3" y="3" /><circle
                  cx="8.5"
                  cy="8.5"
                  r="1.5"
                /><path d="M21 15l-5-5L5 21" /></svg
              >
            </button>
            <input
              bind:this={inputEl}
              class="text-input"
              disabled={!canSend}
              onkeydown={onKeydown}
              placeholder={game?.linked
                ? `Ask Sage about ${game.name || game.exe}…`
                : game
                  ? 'Link this window to ask Sage'
                  : 'No game detected'}
              bind:value={prompt}
            />
            {#if asking}
              <button class="send-btn" aria-label="Stop" onclick={stop} title="Stop" type="button">
                <svg fill="currentColor" height="13" viewBox="0 0 24 24" width="13"
                  ><rect height="14" rx="2" width="14" x="5" y="5" /></svg
                >
              </button>
            {:else}
              <button
                class="send-btn"
                aria-label="Send"
                disabled={!canSend || !prompt.trim()}
                onclick={() => send()}
                title="Send"
                type="button"
              >
                <svg fill="currentColor" height="17" viewBox="0 0 24 24" width="17"
                  ><path d="M3 11l18-8-8 18-2-7-8-3z" /></svg
                >
              </button>
            {/if}
          </div>
          <div class="footer">
            <span>{modelLabel(provider)} · {asking ? 'streaming' : 'Enter to send'}</span>
            <span>{captureHint}</span>
          </div>
        </div>
      </div>
    {:else}
      <!-- translate -->
      <div class="body translate">
        <div class="lang-row">
          <span class="lang-chip">Auto-detect</span>
          <span class="lang-arrow">→</span>
          <span class="lang-chip accent">English</span>
        </div>
        <div class="translate-result">
          {#if translateBusy}
            <div class="thinking"><i></i><i></i><i></i></div>
          {:else if translateError}
            <div style="color: var(--color-err);" class="te-title">{translateError}</div>
          {:else if translateText}
            <div class="translate-text">{translateText}</div>
          {:else}
            <div class="translate-empty">
              {#if !availability.gemini}
                <div class="te-title">Translation needs a Gemini key.</div>
                <div class="te-sub">Add a Gemini key in Settings.</div>
              {:else}
                <div class="te-title">No foreign text captured yet.</div>
                <div class="te-sub">
                  Ctrl+Shift+T opens this tab. Press Enter or click Capture &amp; translate to send
                  the game's screen.
                </div>
              {/if}
            </div>
          {/if}
        </div>
        <div class="translate-actions">
          <button
            bind:this={translateBtn}
            class="recapture live"
            disabled={translateBusy || !game?.linked || !availability.gemini}
            onclick={runTranslate}
            type="button">Capture &amp; translate</button
          >
          <button
            class="recapture live"
            disabled={!translateText}
            onclick={copyTranslation}
            type="button">Copy</button
          >
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .overlay-root {
    width: 100vw;
    height: 100vh;
    padding: 12px;
    box-sizing: border-box;
    display: flex;
    font-family: var(--font-body);
    color: var(--color-t-hi);
    background: transparent;
  }
  * {
    box-sizing: border-box;
  }
  .panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    border-radius: 16px;
    overflow: hidden;
    background: rgba(17, 17, 21, 0.9);
    backdrop-filter: blur(30px);
    border: 1px solid color-mix(in oklab, var(--accent) 22%, var(--color-line));
    box-shadow:
      0 24px 70px -20px rgba(0, 0, 0, 0.7),
      inset 0 1px 0 rgba(255, 255, 255, 0.04);
  }

  /* titlebar */
  .titlebar {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 13px 14px;
    cursor: move;
    user-select: none;
    border-bottom: 1px solid var(--color-line-2);
  }
  .logo {
    position: relative;
    width: 22px;
    height: 22px;
    flex-shrink: 0;
  }
  .logo::before {
    content: '';
    position: absolute;
    inset: 0;
    border-radius: 50%;
    background: radial-gradient(
      circle at 50% 38%,
      #fff 0%,
      color-mix(in oklab, var(--accent) 85%, white) 26%,
      var(--accent) 60%,
      color-mix(in oklab, var(--accent) 40%, transparent) 82%,
      transparent 100%
    );
    box-shadow: 0 0 16px -2px var(--accent);
  }
  .logo::after {
    content: '';
    position: absolute;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: rgba(255, 255, 255, 0.9);
    top: 20%;
    right: 22%;
  }
  .wordmark {
    font-family: var(--font-display);
    font-weight: 700;
    font-size: 14px;
    letter-spacing: 0.14em;
    color: var(--color-t-hi);
  }
  .drag-chip {
    font-family: var(--font-mono);
    font-size: 9px;
    letter-spacing: 0.08em;
    color: var(--color-t-lo);
    padding: 2px 6px;
    border: 1px solid var(--color-line);
    border-radius: 6px;
  }
  .title-actions {
    margin-left: auto;
    display: flex;
    gap: 6px;
  }
  .icon-btn {
    width: 30px;
    height: 30px;
    display: grid;
    place-items: center;
    border-radius: 9px;
    border: 1px solid var(--color-line);
    background: rgba(255, 255, 255, 0.02);
    color: var(--color-t-mid);
    cursor: pointer;
  }
  .icon-btn:hover {
    color: var(--color-t-hi);
    background: rgba(255, 255, 255, 0.06);
  }

  /* detected game */
  .gamebar {
    display: flex;
    align-items: center;
    gap: 11px;
    padding: 12px 14px;
  }
  .game-tile {
    width: 34px;
    height: 34px;
    border-radius: 9px;
    flex-shrink: 0;
    background: linear-gradient(135deg, color-mix(in oklab, var(--accent) 55%, #17171b), #101013);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.08);
  }
  .game-tile.muted {
    background: var(--color-ink-2);
  }
  .game-meta {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .game-title {
    font-weight: 600;
    font-size: 13.5px;
    color: var(--color-t-hi);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .game-title.dim {
    color: var(--color-t-mid);
  }
  .game-exe {
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--color-t-lo);
  }
  .linked-pill {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 500;
    color: var(--accent);
    background: color-mix(in oklab, var(--accent) 14%, transparent);
    border: 1px solid color-mix(in oklab, var(--accent) 30%, transparent);
  }
  .linked-pill .d {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    box-shadow: 0 0 6px var(--accent);
  }
  .link-btn {
    margin-left: auto;
    flex-shrink: 0;
    max-width: 55%;
    padding: 5px 10px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--color-t-hi);
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid var(--color-line);
    cursor: pointer;
  }
  .link-btn:hover {
    border-color: color-mix(in oklab, var(--accent) 40%, transparent);
    background: color-mix(in oklab, var(--accent) 12%, transparent);
  }

  /* tabs + provider */
  .tabrow {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 4px 14px 14px;
  }
  .tabs {
    display: flex;
    gap: 3px;
    padding: 3px;
    border-radius: 11px;
    background: var(--color-ink-2);
    border: 1px solid var(--color-line-2);
  }
  .tab {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 7px 13px;
    border-radius: 8px;
    font-size: 12.5px;
    font-weight: 500;
    color: var(--color-t-mid);
    cursor: pointer;
    border: 0;
    background: transparent;
  }
  .tab.active {
    color: var(--accent);
    background: color-mix(in oklab, var(--accent) 14%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--accent) 26%, transparent);
  }
  .provider-pill {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    border-radius: 10px;
    background: var(--color-ink-2);
    border: 1px solid var(--color-line);
    color: var(--color-t-hi);
    font-size: 12.5px;
    font-weight: 500;
    cursor: pointer;
  }
  .provider-pill:disabled {
    cursor: default;
    opacity: 0.6;
  }
  .provider-note {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    color: var(--color-t-mid);
    font-size: 12.5px;
    font-weight: 500;
  }
  .prov-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }
  .caret {
    color: var(--color-t-mid);
    font-size: 10px;
  }

  /* provider dropdown */
  .dropdown {
    position: absolute;
    top: 46px;
    right: 14px;
    width: 232px;
    z-index: 20;
    background: var(--color-ink-1);
    border: 1px solid var(--color-line);
    border-radius: 13px;
    padding: 6px;
    box-shadow: 0 20px 50px -12px rgba(0, 0, 0, 0.75);
    animation: fade-up 0.14s ease both;
  }
  .dropdown-head {
    font-family: var(--font-mono);
    font-size: 9px;
    letter-spacing: 0.14em;
    color: var(--color-t-lo);
    text-transform: uppercase;
    padding: 8px 10px 7px;
  }
  .prov-row {
    display: flex;
    align-items: center;
    gap: 11px;
    width: 100%;
    padding: 9px 10px;
    border-radius: 9px;
    cursor: pointer;
    border: 0;
    background: transparent;
    text-align: left;
  }
  .prov-row:hover {
    background: rgba(255, 255, 255, 0.03);
  }
  .pdot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .pmeta {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .pname {
    font-size: 13px;
    font-weight: 500;
    color: var(--color-t-hi);
  }
  .pmodel {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--color-t-lo);
  }
  .pcheck {
    margin-left: auto;
    color: var(--accent);
    display: grid;
    place-items: center;
  }

  /* chat body */
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .msglist {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 6px 14px 10px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .msg {
    display: flex;
    gap: 10px;
    max-width: 100%;
  }
  .avatar {
    position: relative;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    flex-shrink: 0;
    margin-top: 2px;
    background: radial-gradient(
      circle at 50% 38%,
      #fff 0%,
      color-mix(in oklab, var(--accent) 85%, white) 26%,
      var(--accent) 60%,
      transparent 100%
    );
    box-shadow: 0 0 14px -3px var(--accent);
  }
  .bubble {
    padding: 11px 14px;
    border-radius: 13px;
    font-size: 13.5px;
    line-height: 1.5;
    color: var(--color-t-hi);
    white-space: pre-wrap;
    word-break: break-word;
  }
  /* The intro text wraps in the template; pre-wrap would render those breaks. */
  .bubble.intro {
    white-space: normal;
  }
  .msg.sage .bubble {
    background: var(--color-ink-2);
    border: 1px solid var(--color-line-2);
    border-top-left-radius: 5px;
  }
  .msg.user {
    flex-direction: column;
    align-items: flex-end;
  }
  .msg.user .bubble {
    background: color-mix(in oklab, var(--accent) 16%, var(--color-ink-3));
    border: 1px solid color-mix(in oklab, var(--accent) 24%, transparent);
    border-top-right-radius: 5px;
  }
  .meta {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--color-t-lo);
    margin-top: 7px;
    letter-spacing: 0.04em;
  }
  .frame-chip {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 4px 9px 4px 4px;
    border-radius: 8px;
    background: var(--color-ink-3);
    border: 1px solid var(--color-line);
    font-family: var(--font-mono);
    font-size: 9px;
    letter-spacing: 0.04em;
    color: var(--color-t-mid);
    margin-bottom: 7px;
  }
  .frame-chip .thumb {
    width: 24px;
    height: 16px;
    border-radius: 4px;
    background: linear-gradient(135deg, color-mix(in oklab, var(--accent) 52%, #17171b), #101013);
  }
  .caret-blink {
    display: inline-block;
    width: 2px;
    height: 0.95em;
    background: var(--accent);
    margin-left: 2px;
    vertical-align: text-bottom;
    animation: blink 1s steps(2) infinite;
  }
  @keyframes blink {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0;
    }
  }
  .thinking {
    display: inline-flex;
    gap: 4px;
    align-items: center;
  }
  .thinking i {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--color-t-mid);
    animation: pulse-soft 1.2s ease-in-out infinite;
  }
  .thinking i:nth-child(2) {
    animation-delay: 0.18s;
  }
  .thinking i:nth-child(3) {
    animation-delay: 0.36s;
  }

  /* suggested prompt chips */
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    padding-left: 36px;
  }
  .chip {
    padding: 8px 12px;
    border-radius: 10px;
    font-size: 12px;
    color: var(--color-t-mid);
    background: rgba(255, 255, 255, 0.02);
    border: 1px solid var(--color-line);
    cursor: pointer;
  }
  .chip:hover {
    color: var(--color-t-hi);
    border-color: color-mix(in oklab, var(--accent) 34%, transparent);
  }

  /* input row + footer */
  .inputbar {
    padding: 10px 14px 8px;
    border-top: 1px solid var(--color-line-2);
  }
  .inputrow {
    display: flex;
    align-items: center;
    gap: 9px;
  }
  .attach-btn {
    width: 40px;
    height: 40px;
    flex-shrink: 0;
    display: grid;
    place-items: center;
    border-radius: 11px;
    border: 1px solid color-mix(in oklab, var(--accent) 40%, transparent);
    background: color-mix(in oklab, var(--accent) 12%, transparent);
    color: var(--accent);
    cursor: pointer;
  }
  .attach-btn.off {
    border-color: var(--color-line);
    background: rgba(255, 255, 255, 0.02);
    color: var(--color-t-mid);
  }
  .attach-btn:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .text-input {
    flex: 1;
    min-width: 0;
    height: 40px;
    border-radius: 11px;
    border: 1px solid var(--color-line);
    background: var(--color-ink-2);
    color: var(--color-t-hi);
    font-family: var(--font-body);
    font-size: 13px;
    padding: 0 14px;
    outline: none;
  }
  .text-input::placeholder {
    color: var(--color-t-lo);
  }
  .text-input:disabled {
    opacity: 0.6;
  }
  .send-btn {
    width: 40px;
    height: 40px;
    flex-shrink: 0;
    display: grid;
    place-items: center;
    border-radius: 11px;
    border: 0;
    background: var(--accent);
    color: #0b0b0d;
    cursor: pointer;
  }
  .send-btn:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 8px 2px 2px;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--color-t-lo);
  }

  /* translate view */
  .translate {
    padding: 4px 14px 14px;
    gap: 14px;
  }
  .lang-row {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .lang-chip {
    padding: 8px 12px;
    border-radius: 10px;
    font-size: 12px;
    color: var(--color-t-mid);
    background: var(--color-ink-2);
    border: 1px solid var(--color-line);
  }
  .lang-chip.accent {
    color: var(--accent);
    border-color: color-mix(in oklab, var(--accent) 32%, transparent);
    background: color-mix(in oklab, var(--accent) 12%, transparent);
  }
  .lang-arrow {
    color: var(--color-t-lo);
  }
  .translate-empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    gap: 6px;
  }
  .te-title {
    font-size: 13.5px;
    color: var(--color-t-mid);
  }
  .te-sub {
    font-size: 12px;
    color: var(--color-t-lo);
  }
  .translate-actions {
    display: flex;
    justify-content: center;
    gap: 10px;
  }
  .recapture {
    padding: 9px 16px;
    border-radius: 11px;
    border: 1px solid var(--color-line);
    background: var(--color-ink-2);
    color: var(--color-t-mid);
    font-size: 12.5px;
    font-weight: 500;
    cursor: not-allowed;
    opacity: 0.7;
  }
  .recapture.live {
    cursor: pointer;
    opacity: 1;
    color: var(--color-t-hi);
  }
  .recapture.live:hover {
    border-color: color-mix(in oklab, var(--accent) 34%, transparent);
  }
  .recapture.live:disabled {
    cursor: default;
    opacity: 0.45;
  }
  .translate-result {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
  }
  .translate-text {
    font-size: 13.5px;
    line-height: 1.55;
    color: var(--color-t-hi);
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
