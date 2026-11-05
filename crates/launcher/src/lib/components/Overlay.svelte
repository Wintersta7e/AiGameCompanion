<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { on as onEvent } from 'svelte/events';
  import { invoke, Channel } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { hashHue } from '../utils/accent';
  import {
    chatViewAction,
    saveStatusText,
    type ChatView,
    type SaveState,
    type SavedExchange,
  } from '../utils/chat';
  import type { SearchReport } from '../utils/search';
  import { ANOTHER_HINT_TURN, FULL_ANSWER_TURN, showHintChips } from '../utils/hints';
  import { sendsQuestion } from '../utils/keys';
  import { unsavedPick } from '../utils/pick';
  import { PROVIDERS, modelName, type ModelNames, type Provider } from '../stores/companion.svelte';
  import type { LauncherSettings } from '../settings';
  import Markdown from './Markdown.svelte';
  import {
    NO_SHOT,
    afterSend,
    drawFailed,
    drawn,
    heldShot,
    received,
    replaced,
    sendBlocked,
    sendableShot,
    takeFailed,
    taking,
    translatePreview,
    type HotkeyAction,
    type HotkeyTake,
    type ShotHeld,
    type ShotInfo,
    type ShotSlot,
    type ShotState,
  } from '../utils/shots';

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
  type SageKind = 'chunk' | 'done' | 'error';
  interface SageEvent {
    kind: SageKind;
    requestId: number;
    conversationId: number;
    // The number of the chat on screen the request was admitted for.
    displayGeneration: number;
    text?: string;
    message?: string;
    // Whether the finished answer was saved.
    save?: SaveState;
    search?: SearchReport;
  }
  interface Msg {
    role: 'user' | 'assistant';
    content: string;
    model?: string;
    screenshot?: boolean;
    // The sent screenshot, chip-sized; absent when none was sent.
    thumb?: ImageBitmap;
    streaming?: boolean;
    // Set only when the answer finished ("done"); stopped and failed ones never are.
    complete?: boolean;
    // Set once the answer is too long or too slow to format; it then stays plain text.
    plain?: boolean;
    // Why the answer failed; shown on its own line, never part of `content`.
    error?: string;
    // Asked with hints first on; the follow-up chips show only after such an answer.
    hinted?: boolean;
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
  const pick = unsavedPick<Provider>();
  let dropdownOpen = $state(false);
  let tab = $state<'chat' | 'translate'>('chat');
  // The chat screenshot: taken by the image button, shown above the input, and
  // sent with the next question only once it is drawn.
  let chatShot = $state.raw<ShotState>(NO_SHOT);
  let chatCanvas = $state<HTMLCanvasElement | null>(null);
  // The drawn preview; freed whenever its shot is replaced, removed or sent.
  let chatBitmap = $state.raw<ImageBitmap | null>(null);
  // The translate screenshot: taken by the translate button, shown above it,
  // and translated only once it is drawn. It stays shown after it is sent.
  let translateShot = $state.raw<ShotState>(NO_SHOT);
  let translateCanvas = $state<HTMLCanvasElement | null>(null);
  let translateBitmap = $state.raw<ImageBitmap | null>(null);
  let shotSeq = 0;
  // Per slot, the last take a capture hotkey started; its result arrives as
  // `shot-held` with the same take number. Rust numbers takes from 1.
  const hotkeyTake: Record<ShotSlot, HotkeyTake> = {
    chat: { seq: 0, take: 0 },
    translate: { seq: 0, take: 0 },
  };
  // Hints first: answers to progress questions start as a nudge. Each send
  // carries the value shown here, whatever the saved setting says.
  let hintsFirst = $state(true);
  let prompt = $state('');
  let asking = $state(false);
  let messages = $state<Msg[]>([]);
  // The saved chat on screen: Rust's number for it (0 = none), and whether it
  // has arrived -- nothing is sent before it has.
  let displayGeneration = 0;
  let chatReady = $state(false);
  // Whether the chat on screen is saved; null until it arrives.
  let saveState = $state<SaveState | null>(null);
  // Only the latest load_chat or clear_chat call applies its result.
  let chatLoadSeq = 0;
  // Why New chat did not clear the saved chat; shown above the messages until
  // another chat is shown or a question is sent.
  let chatNotice = $state('');
  // The answer whose Copy just succeeded, by index; -1 for none.
  let copiedIndex = $state(-1);

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
  const canSend = $derived(Boolean(game?.linked) && availability[provider] && chatReady);
  const saveStatus = $derived(saveStatusText(saveState, Boolean(game?.linked)));
  const canTakeShot = $derived(Boolean(game?.linked) && !sendBlocked(chatShot));
  // Gemini is the only provider that translates.
  const canTranslate = $derived(
    Boolean(game?.linked) && availability.gemini && !translateBusy && !sendBlocked(translateShot),
  );
  const canRetakeTranslate = $derived(
    Boolean(game?.linked) && !translateBusy && !sendBlocked(translateShot),
  );
  const hintChips = $derived(showHintChips(messages, hintsFirst, asking, canSend));
  // The exe's file name, for the link control ("Link foo.exe ...").
  const exeFile = $derived(game?.exe.split(/[\\/]/u).pop() ?? '');
  // The linked window as the screenshot controls name it.
  const shotName = $derived(game ? game.name || game.exe : '');
  // A sent message's chip shows the screenshot at most twice its 24x16 box.
  const THUMB_W = 48;
  const THUMB_H = 32;

  // How far above the bottom of the chat list still counts as at the bottom.
  const FOLLOW_SLACK = 50;

  // Follow a streaming answer, but only while the user is already at the bottom
  // -- scrolling up to re-read history must not be yanked back down. The check
  // has to happen before the DOM grows, hence $effect.pre.
  $effect.pre(() => {
    // Re-runs on every appended chunk, on every new message, when an answer
    // fails (its error line sits below the text) or finishes (the status line
    // under the input can then wrap, shrinking the list) and when the hint
    // chips show.
    const last = messages.at(-1);
    const growth =
      messages.length +
      (last?.content.length ?? 0) +
      (last?.error?.length ?? 0) +
      (last?.streaming ? 1 : 0) +
      (hintChips ? 1 : 0);
    const el = msglistEl;
    if (!el || growth === 0) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight > FOLLOW_SLACK) return;
    void tick().then(() => {
      el.scrollTop = el.scrollHeight;
    });
  });

  // Where the chat list stood when it was last scrolled: null at its bottom,
  // else its scrollTop. The list is rebuilt, at its top, on every switch back
  // to the Chat tab.
  let chatScrollTop: number | null = null;

  // Svelte action: put a rebuilt chat list back where it stood. And when the
  // list gets shorter (a screenshot preview shows under it), its scroll
  // position stays fixed from the top, which would leave a list that was at
  // its bottom scrolled up and unfollowed. Keep it there.
  function keepBottom(el: HTMLDivElement) {
    el.scrollTop = chatScrollTop ?? el.scrollHeight;
    const remember = () => {
      const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight <= FOLLOW_SLACK;
      chatScrollTop = atBottom ? null : el.scrollTop;
    };
    const stopRemembering = onEvent(el, 'scroll', remember);
    let height = el.clientHeight;
    const observer = new ResizeObserver(() => {
      const wasAtBottom = el.scrollHeight - el.scrollTop - height <= FOLLOW_SLACK;
      if (wasAtBottom && el.clientHeight < height) el.scrollTop = el.scrollHeight;
      height = el.clientHeight;
    });
    observer.observe(el);
    return {
      destroy() {
        observer.disconnect();
        stopRemembering();
      },
    };
  }

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
    pick.picked(p);
    dropdownOpen = false;
    try {
      await invoke('set_active_provider', { provider: p });
      pick.saved();
    } catch {
      /* selection still applies for this session */
    }
  }

  // The switch applies at once and to every later send, even when the backend
  // refuses to remember it (a read-only state file), like a provider pick.
  async function toggleHints() {
    const on = !hintsFirst;
    hintsFirst = on;
    try {
      await invoke('set_hints_first', { on });
    } catch {
      /* the choice still applies for this session */
    }
  }

  async function newChat() {
    const inflight = asking ? activeRequestId : 0;
    // Reset synchronously first so a Send fired during the cancel IPC gap cannot
    // be clobbered by a post-await state reset.
    activeRequestId = 0;
    asking = false;
    conversationId += 1;
    removeShot('chat');
    for (const m of messages) m.thumb?.close();
    messages = [];
    copiedIndex = -1;
    prompt = '';
    // The saved chat on screen is deleted too; nothing is sent until the
    // emptied chat arrives. A later show or link makes the result stale.
    const shown = game?.linked && chatReady ? game : null;
    const seq = shown ? (chatLoadSeq += 1) : 0;
    const generation = displayGeneration;
    if (shown) chatReady = false;
    if (inflight !== 0) {
      try {
        await invoke('cancel_sage', { requestId: inflight });
      } catch {
        /* best effort */
      }
    }
    if (!shown) return;
    const stale = () => seq !== chatLoadSeq || game?.hwnd !== shown.hwnd || game.pid !== shown.pid;
    try {
      const view = await invoke<ChatView>('clear_chat', { displayGeneration: generation });
      if (!stale()) await applyView(view);
    } catch (err) {
      if (stale()) return;
      // Show the chat as it is, then why it was not cleared.
      await reloadChat();
      chatNotice = String(err);
    }
  }

  function dropShot(id: number) {
    void invoke('drop_shot', { id }).catch(() => {
      /* the next hide drops it anyway */
    });
  }

  function shotOf(slot: ShotSlot): ShotState {
    return slot === 'chat' ? chatShot : translateShot;
  }

  function setShot(slot: ShotSlot, next: ShotState) {
    if (slot === 'chat') chatShot = next;
    else translateShot = next;
  }

  // Replace a slot's drawn preview, freeing the old one.
  function setBitmap(slot: ShotSlot, bitmap: ImageBitmap | null) {
    if (slot === 'chat') {
      chatBitmap?.close();
      chatBitmap = bitmap;
    } else {
      translateBitmap?.close();
      translateBitmap = bitmap;
    }
  }

  // Remove: the slot's held shot is dropped and the slot is empty again.
  function removeShot(slot: ShotSlot) {
    const state = shotOf(slot);
    if ('info' in state) dropShot(state.info.id);
    setBitmap(slot, null);
    setShot(slot, NO_SHOT);
  }

  // Draw `bitmap` at its own size; CSS scales the canvas, aspect kept.
  function paint(canvas: HTMLCanvasElement, bitmap: ImageBitmap): boolean {
    const context = canvas.getContext('2d');
    if (!context) return false;
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    context.drawImage(bitmap, 0, 0);
    return true;
  }

  // Svelte action: a chip thumbnail or a preview. A canvas is recreated each
  // time its tab is shown again, so it paints its bitmap on every mount.
  function showBitmap(canvas: HTMLCanvasElement, bitmap: ImageBitmap | null) {
    if (bitmap) paint(canvas, bitmap);
    return {
      update(next: ImageBitmap | null) {
        if (next) paint(canvas, next);
      },
    };
  }

  // Take (or retake) a slot's screenshot. Nothing is sent: it is shown and
  // goes with the next question or translation.
  async function takeShot(slot: ShotSlot) {
    if (!game?.linked || sendBlocked(shotOf(slot))) return;
    const seq = (shotSeq += 1);
    setBitmap(slot, null);
    setShot(slot, taking(seq));
    let info: ShotInfo | null;
    try {
      info = await invoke<ShotInfo | null>('take_shot', { slot, editRegion: false });
    } catch (err) {
      setShot(slot, takeFailed(shotOf(slot), seq, String(err)));
      return;
    }
    if (!info) {
      // A capture hotkey's take replaced it; that one is shown instead.
      setShot(slot, replaced(shotOf(slot), seq));
      return;
    }
    const next = received(shotOf(slot), info, seq);
    if (next === shotOf(slot)) {
      // Replaced or reset while it was taken: nothing will show it.
      dropShot(info.id);
      return;
    }
    setShot(slot, next);
    await drawShot(slot, info.id);
  }

  // Fetch the held shot `id` and draw it into the slot's preview.
  async function drawShot(slot: ShotSlot, id: number) {
    let bitmap: ImageBitmap;
    try {
      const png = await invoke<ArrayBuffer>('shot_png', { id });
      bitmap = await createImageBitmap(new Blob([png], { type: 'image/png' }));
    } catch (err) {
      dropShot(id);
      setShot(slot, drawFailed(shotOf(slot), id, String(err)));
      return;
    }
    await tick();
    if (drawn(shotOf(slot), id) === shotOf(slot)) {
      // Replaced or reset while it loaded.
      bitmap.close();
      return;
    }
    // A tab switch unmounts the other tab's canvas; showBitmap paints the
    // preview when that tab shows again, so only a canvas that cannot paint
    // fails the shot.
    const canvas = slot === 'chat' ? chatCanvas : translateCanvas;
    if (canvas && !paint(canvas, bitmap)) {
      bitmap.close();
      dropShot(id);
      setShot(slot, drawFailed(shotOf(slot), id, 'the preview could not be drawn'));
      return;
    }
    setBitmap(slot, bitmap);
    setShot(slot, drawn(shotOf(slot), id));
  }

  // A capture hotkey already took the screenshot; it shows up as `shot-held`.
  function hotkeyTaking(slot: ShotSlot, take: number) {
    const seq = (shotSeq += 1);
    hotkeyTake[slot] = { seq, take };
    setBitmap(slot, null);
    setShot(slot, taking(seq));
  }

  // A hotkey's take finished: show it as a button take would be shown.
  async function onShotHeld(held: ShotHeld) {
    const { slot } = held;
    const next = heldShot(shotOf(slot), held, hotkeyTake[slot]);
    if (next === shotOf(slot)) {
      // A newer take or a reset since the hotkey: nothing will show it.
      if (held.shot) dropShot(held.shot.id);
      return;
    }
    setShot(slot, next);
    if (held.shot) await drawShot(slot, held.shot.id);
    if (slot === 'translate') {
      // The button was disabled while the screenshot was taken.
      await tick();
      translateBtn?.focus();
    }
  }

  // The chip's image: the preview scaled into THUMB_W x THUMB_H. The full
  // bitmap is freed either way.
  async function makeThumb(full: ImageBitmap): Promise<ImageBitmap | null> {
    const scale = Math.min(THUMB_W / full.width, THUMB_H / full.height, 1);
    try {
      return await createImageBitmap(full, {
        resizeWidth: Math.max(1, Math.round(full.width * scale)),
        resizeHeight: Math.max(1, Math.round(full.height * scale)),
        resizeQuality: 'medium',
      });
    } catch {
      return null;
    } finally {
      full.close();
    }
  }

  function onWindowPointerDown(event: PointerEvent) {
    if (!dropdownOpen) return;
    const target = event.target as HTMLElement;
    if (!target.closest('.provider-pill') && !target.closest('.dropdown')) {
      dropdownOpen = false;
    }
  }

  // A saved exchange as the chat shows it: the question, with the chip of a
  // sent screenshot (never the image), and its finished answer.
  const savedMessages = (exchange: SavedExchange): Msg[] => [
    { role: 'user', content: exchange.question, screenshot: exchange.screenshot },
    {
      role: 'assistant',
      content: exchange.answer,
      complete: true,
      model: exchange.model || modelLabel(exchange.provider),
    },
  ];

  // Show the chat `load_chat` returned for the window on screen. The chat
  // already shown only updates its status: its messages, and where the list
  // is scrolled, stay as they are.
  async function applyView(view: ChatView | null) {
    const action = chatViewAction(displayGeneration, view);
    if (action !== 'status') {
      for (const m of messages) m.thumb?.close();
      messages = view && action === 'replace' ? view.exchanges.flatMap(savedMessages) : [];
      asking = false;
      activeRequestId = 0;
      // A late answer or thumbnail of the chat replaced here must not land in this one.
      conversationId += 1;
      copiedIndex = -1;
      chatNotice = '';
      // Another chat opens at its latest message.
      chatScrollTop = null;
    }
    displayGeneration = view?.displayGeneration ?? 0;
    chatReady = view !== null;
    saveState = view?.save ?? null;
    await tick();
    if (action !== 'status' && msglistEl) msglistEl.scrollTop = msglistEl.scrollHeight;
    // The input was disabled until the chat arrived, so a focus at the show
    // did not take; a staged quick ask must still send on Enter.
    if (tab === 'chat' && document.activeElement === document.body) inputEl?.focus();
  }

  // Load the chat of the window on screen, on every show and after a link. A
  // window that is not linked shows no chat.
  async function reloadChat() {
    const seq = (chatLoadSeq += 1);
    const shown = game;
    if (!shown?.linked) {
      await applyView(null);
      return;
    }
    const view = await invoke<ChatView | null>('load_chat', {
      hwnd: shown.hwnd,
      pid: shown.pid,
    }).catch(() => null);
    // A later show or link, or another window, makes this result stale.
    if (seq !== chatLoadSeq || game?.hwnd !== shown.hwnd || game.pid !== shown.pid) return;
    await applyView(view);
  }

  // Send a question, or a recap of the chat so far; Rust adds the saved chat
  // as its history.
  async function send(text?: string, opts: { recap?: boolean } = {}) {
    const isRecap = opts.recap === true;
    const question = (text ?? prompt).trim();
    if (!question || asking || !canSend || (!isRecap && sendBlocked(chatShot))) return;

    const id = (nextRequestId += 1);
    const convo = conversationId;
    activeRequestId = id;
    chatNotice = '';
    // Only a drawn screenshot goes, never with a recap; its preview becomes
    // the chip's thumbnail. A recap leaves the held screenshot as it is.
    const shotId = isRecap ? null : sendableShot(chatShot);
    const full = shotId === null ? null : chatBitmap;
    if (full) chatBitmap = null;
    if (!isRecap) chatShot = afterSend(chatShot);
    const thumb = full ? makeThumb(full) : null;

    messages = [
      ...messages,
      { role: 'user', content: question },
      {
        role: 'assistant',
        content: '',
        model: modelLabel(provider),
        streaming: true,
        hinted: hintsFirst && !isRecap,
      },
    ];
    const idx = messages.length - 1;
    streamIndex = idx;
    prompt = '';
    asking = true;

    // The chip says whether the screenshot went: `ask_sage` returns whether
    // the request started, and so whether its screenshot was claimed, so a
    // failed or stopped answer still shows the chip. A thumbnail that
    // arrives after New chat is freed.
    const asked = messages[idx - 1];
    const settleShot = (sent: boolean) => {
      if (sent && asked && thumb) asked.screenshot = true;
      void thumb?.then((bitmap) => {
        if (sent && asked && bitmap && convo === conversationId) asked.thumb = bitmap;
        else bitmap?.close();
      });
    };

    const channel = new Channel<SageEvent>();
    channel.onmessage = (event) => {
      // An event for another chat than the one on screen is dropped; when it
      // is the running request's, the chat on screen is stale, so reload it.
      if (event.displayGeneration !== displayGeneration) {
        if (event.requestId === activeRequestId) void reloadChat();
        return;
      }
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
        if (event.save) saveState = event.save;
      } else {
        bubble.error = event.message ?? 'Unknown error';
        bubble.streaming = false;
        asking = false;
      }
    };

    try {
      // A refused request reports why on the channel, as its one error.
      const started = await invoke('ask_sage', {
        requestId: id,
        conversationId: convo,
        provider,
        channel,
        question,
        shotId,
        search: false,
        hints: hintsFirst,
        recap: isRecap,
      });
      settleShot(started === true);
    } catch (err) {
      settleShot(false);
      // Same guard as the channel handler: a rejection that lands after New chat
      // or a newer Send must not write into the current conversation's bubble.
      if (id !== activeRequestId || convo !== conversationId) return;
      const bubble = messages[idx];
      if (bubble) {
        bubble.error = String(err);
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
    if (sendsQuestion(event)) {
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
    await reloadChat();
    await tick();
    inputEl?.focus();
  }

  // With a drawn screenshot, translate exactly that one; without one, the same
  // button takes a screenshot and sends nothing.
  async function runTranslate() {
    if (!canTranslate) return;
    const shotId = sendableShot(translateShot);
    if (shotId === null) {
      await takeShot('translate');
      // The button was disabled while the screenshot was taken.
      await tick();
      translateBtn?.focus();
      return;
    }
    translateShot = afterSend(translateShot);
    translateBusy = true;
    translateError = '';
    try {
      const res = await invoke<{ text: string; cut_short: string | null }>('translate_screen', {
        shotId,
      });
      translateText = res.text;
      // A translation cut short keeps its text with the reason under it.
      translateError = res.cut_short ?? '';
    } catch (err) {
      translateError = String(err);
      translateText = '';
    } finally {
      translateBusy = false;
    }
  }

  // The hotkey stages the question with its screenshot: nothing is sent, and
  // no answer in progress is cancelled, until the user presses Enter or clicks.
  async function runQuickAsk() {
    tab = 'chat';
    prompt = QUICK_ASK;
    await tick();
    inputEl?.focus();
  }

  // A finished or stopped answer with text can be copied.
  const copyable = (m: Msg) => !m.streaming && m.content.trim() !== '';

  // Copies the answer as it arrived, Markdown included: never the error line or
  // the rendered text. "Copied" shows only once the clipboard took it.
  async function copyAnswer(text: string, index: number) {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      return;
    }
    copiedIndex = index;
    setTimeout(() => {
      if (copiedIndex === index) copiedIndex = -1;
    }, 1500);
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
      const settings = await invoke<LauncherSettings>('get_settings');
      // The saved provider is kept even when unavailable. An empty value (a
      // new install, nothing picked yet) or an unknown one (a hand-edited
      // state file) is no saved provider, so the first available one is used.
      const saved = settings.active_provider;
      savedProvider = pick.after(saved && saved in PROVIDERS ? (saved as Provider) : null);
    } catch {
      /* defaults apply */
    }
    savedProviderLoaded = true;
    await refreshProviders();
  }

  // Read once, at mount. Re-reading it on a provider change would put back a
  // value the backend refused to store, and the panel would then show one
  // value while the last send used another.
  async function loadHintsFirst() {
    try {
      const settings = await invoke<LauncherSettings>('get_settings');
      if (typeof settings.hints_first === 'boolean') hintsFirst = settings.hints_first;
    } catch {
      /* the default applies */
    }
  }

  onMount(() => {
    // Only the overlay window mounts this; keep its surface transparent.
    document.documentElement.style.background = 'transparent';
    document.body.style.background = 'transparent';

    void loadSavedProvider();
    void loadHintsFirst();

    const listeners = [
      listen<GameInfo>('overlay-status', (event) => {
        // A show: the backend dropped every held screenshot at the hide before.
        setBitmap('chat', null);
        chatShot = NO_SHOT;
        setBitmap('translate', null);
        translateShot = NO_SHOT;
        game = event.payload;
        void reloadChat();
        // The overlay just became visible: CLI detection has had time to finish.
        if (savedProviderLoaded) void refreshProviders();
        // The window is shown and hidden, never remounted, so the input has to
        // be focused on every show -- the Rust side only focuses the window.
        // Wait for the DOM: `game` above flips canSend, and a still-disabled
        // input silently refuses focus.
        void tick().then(() => inputEl?.focus());
      }),
      listen('target-window-closed', () => {
        // The window this panel names was found closed, so nothing may use it.
        game = null;
      }),
      listen('providers-changed', () => {
        void loadSavedProvider();
      }),
      listen('saved-chats-changed', () => {
        void reloadChat();
      }),
      listen<HotkeyAction>('translate-request', (event) => {
        // Stage only: Enter on the focused button, or a click, runs it.
        tab = 'translate';
        if (event.payload.take !== null) hotkeyTaking('translate', event.payload.take);
        void tick().then(() => translateBtn?.focus());
      }),
      listen<HotkeyAction>('quick-ask', (event) => {
        if (event.payload.take !== null) hotkeyTaking('chat', event.payload.take);
        void runQuickAsk();
      }),
      listen<ShotHeld>('shot-held', (event) => {
        void onShotHeld(event.payload);
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
          title="New chat — deletes this game's saved chat"
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
        <div bind:this={msglistEl} class="msglist" use:keepBottom>
          {#if chatNotice}
            <div style="color: var(--color-err);" class="chat-notice">{chatNotice}</div>
          {/if}
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
                  Linked to {game.name || game.exe}. Ask me anything, or tap a prompt below. Take a
                  screenshot with the image button (or Ctrl+Shift+A); you see it before anything is
                  sent.
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
                    <span class="frame-chip"
                      >{#if m.thumb}<canvas class="thumb" use:showBitmap={m.thumb}
                        ></canvas>{:else}<span class="thumb"></span>{/if}frame · WGC</span
                    >
                  {/if}
                  <div class="bubble">{m.content}</div>
                </div>
              {:else}
                <div class="msg sage">
                  <span class="avatar"></span>
                  <div class="sage-col">
                    <div class="bubble">
                      <Markdown
                        complete={m.complete === true}
                        content={m.content}
                        onplain={() => {
                          m.plain = true;
                        }}
                        plain={m.plain === true}
                        streaming={m.streaming === true}
                      />
                      {#if m.streaming && !m.content}
                        <span class="thinking"><i></i><i></i><i></i></span>
                      {/if}
                      {#if m.error}
                        <div class="answer-error" class:spaced={m.content.trim() !== ''}>
                          [error] {m.error}
                        </div>
                      {/if}
                    </div>
                    {#if copyable(m) || (m.model && (m.content || !m.streaming))}
                      <div class="meta">
                        {m.model}{m.streaming ? ' · streaming' : ''}{#if copyable(m)}<button
                            class="copy-btn"
                            onclick={() => copyAnswer(m.content, i)}
                            type="button">{copiedIndex === i ? 'Copied' : 'Copy'}</button
                          >{/if}
                      </div>
                    {/if}
                  </div>
                </div>
              {/if}
            {/each}
            {#if hintChips}
              <div class="chips">
                <button class="chip" onclick={() => send(ANOTHER_HINT_TURN)} type="button"
                  >Another hint</button
                >
                <button class="chip" onclick={() => send(FULL_ANSWER_TURN)} type="button"
                  >Full answer</button
                >
              </div>
            {/if}
          {/if}
        </div>

        <div class="inputbar">
          {#if chatShot.kind === 'fetched' || chatShot.kind === 'shown'}
            <div class="shot-preview">
              <div aria-label="Screenshot of {shotName} that will be sent" role="img">
                <canvas bind:this={chatCanvas} class="shot-canvas" use:showBitmap={chatBitmap}
                ></canvas>
              </div>
              <div class="shot-actions">
                <button
                  class="recapture live"
                  disabled={!canTakeShot}
                  onclick={() => takeShot('chat')}
                  type="button">Retake</button
                >
                <button
                  class="recapture live"
                  onclick={() => {
                    removeShot('chat');
                  }}
                  type="button">Remove</button
                >
              </div>
              <div class="shot-hint">To capture a new moment, hide Sage and open it again.</div>
            </div>
          {:else if chatShot.kind === 'failed'}
            <div class="shot-preview">
              <div class="shot-error">The screenshot could not be taken: {chatShot.message}</div>
              <div class="shot-actions">
                <button
                  class="recapture live"
                  disabled={!canTakeShot}
                  onclick={() => takeShot('chat')}
                  type="button">Retake</button
                >
              </div>
            </div>
          {/if}
          <div class="inputrow">
            <button
              class="attach-btn"
              class:off={chatShot.kind !== 'shown'}
              aria-label="Take screenshot of {shotName}"
              disabled={!canTakeShot}
              onclick={() => takeShot('chat')}
              title="Take screenshot of {shotName}"
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
            <button
              class="attach-btn hints-btn"
              class:off={!hintsFirst}
              aria-label="Hints first"
              aria-pressed={hintsFirst}
              onclick={toggleHints}
              title="Hints first"
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
                ><path
                  d="M9 18h6M10 21h4M12 3a6 6 0 0 0-3.6 10.8c.6.5 1 1.2 1.1 2V16h5v-.2c.1-.8.5-1.5 1.1-2A6 6 0 0 0 12 3z"
                /></svg
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
                disabled={!canSend || !prompt.trim() || sendBlocked(chatShot)}
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
            {#if saveStatus}<span>{saveStatus}</span>{/if}
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
          {:else if translateText || translateError}
            {#if translateText}
              <div class="translate-text">{translateText}</div>
            {/if}
            {#if translateError}
              <div style="color: var(--color-err);" class="te-title">{translateError}</div>
            {/if}
          {:else if !availability.gemini}
            <div class="translate-empty">
              <div class="te-title">Translation needs a Gemini key.</div>
              <div class="te-sub">Add a Gemini key in Settings.</div>
            </div>
          {:else if !translatePreview(translateShot)}
            <div class="translate-empty">
              <div class="te-title">No foreign text captured yet.</div>
              <div class="te-sub">
                Ctrl+Shift+T takes a screenshot of the linked game and shows it here; press Enter or
                Translate to send it.
              </div>
            </div>
          {/if}
        </div>
        {#if translatePreview(translateShot)}
          <div class="shot-preview wide">
            <div aria-label="Screenshot of {shotName} that will be sent" role="img">
              <canvas
                bind:this={translateCanvas}
                class="shot-canvas"
                use:showBitmap={translateBitmap}
              ></canvas>
            </div>
            <div class="shot-actions">
              <button
                class="recapture live"
                disabled={!canRetakeTranslate}
                onclick={() => takeShot('translate')}
                type="button">Retake</button
              >
              <button
                class="recapture live"
                disabled={translateBusy}
                onclick={() => {
                  removeShot('translate');
                }}
                type="button">Remove</button
              >
            </div>
            <div class="shot-hint">To capture a new moment, hide Sage and open it again.</div>
          </div>
        {:else if translateShot.kind === 'failed'}
          <div class="shot-preview wide">
            <div class="shot-error">The screenshot could not be taken: {translateShot.message}</div>
            <div class="shot-actions">
              <button
                class="recapture live"
                disabled={!canRetakeTranslate}
                onclick={() => takeShot('translate')}
                type="button">Retake</button
              >
            </div>
          </div>
        {/if}
        <div class="translate-actions">
          <button
            bind:this={translateBtn}
            class="recapture live"
            disabled={!canTranslate}
            onclick={runTranslate}
            type="button"
            >{sendableShot(translateShot) === null ? 'Take screenshot' : 'Translate'}</button
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
    overflow-wrap: anywhere;
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
  .copy-btn {
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.04em;
    color: var(--color-t-mid);
    background: none;
    border: 0;
    padding: 0;
    margin-left: 8px;
    cursor: pointer;
  }
  .copy-btn:hover {
    color: var(--color-t-hi);
  }
  .answer-error {
    white-space: pre-wrap;
  }
  .chat-notice {
    font-size: 12px;
  }
  .answer-error.spaced {
    margin-top: 8px;
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
  .frame-chip canvas.thumb {
    object-fit: contain;
  }
  /* the chat screenshot, shown before it is sent */
  .shot-preview {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
    margin-bottom: 9px;
  }
  .shot-canvas {
    display: block;
    width: auto;
    height: auto;
    max-width: 100%;
    max-height: 30vh;
    border-radius: 8px;
    border: 1px solid var(--color-line);
  }
  .shot-actions {
    display: flex;
    gap: 8px;
  }
  /* the translate screenshot spans the panel above its actions */
  .shot-preview.wide {
    align-items: stretch;
    margin-bottom: 0;
  }
  .shot-preview.wide .shot-canvas {
    width: 100%;
    object-fit: contain;
  }
  .shot-hint {
    font-size: 11px;
    color: var(--color-t-mid);
  }
  .shot-error {
    white-space: pre-wrap;
    font-size: 12px;
    color: var(--color-err);
  }
  /* A wide code block or table scrolls inside the answer, never widens the panel. */
  .sage-col {
    min-width: 0;
  }
  /* The answer sets its own wrapping; the template's line breaks around it must not render. */
  .sage-col .bubble {
    white-space: normal;
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
    overflow-wrap: anywhere;
  }
</style>
