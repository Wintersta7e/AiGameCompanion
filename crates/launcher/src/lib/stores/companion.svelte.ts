/**
 * Companion (AI provider) selection for the launcher UI. Mirrors the overlay's
 * persisted `active_provider`; `setProvider` persists via `set_active_provider`.
 */

import { invoke } from '@tauri-apps/api/core';

export type Provider = 'gemini' | 'claude' | 'openai';

export interface ProviderMeta {
  label: string;
  dot: string;
}

// No model names here: `available_providers` reports the model each provider uses.
export const PROVIDERS: Record<Provider, ProviderMeta> = {
  gemini: { label: 'Gemini', dot: '#5b9bff' },
  claude: { label: 'Claude', dot: '#d97757' },
  openai: { label: 'OpenAI', dot: '#10a37f' },
};

const PROVIDER_ORDER = Object.keys(PROVIDERS) as Provider[];

/** The model names `available_providers` reports; "" means the CLI's own default. */
export interface ModelNames {
  gemini_model: string;
  claude_model: string;
  openai_model: string;
}

const MODEL_KEYS = {
  gemini: 'gemini_model',
  claude: 'claude_model',
  openai: 'openai_model',
} as const;

/** The label for the model `p` answers with. */
export function modelName(names: ModelNames, p: Provider): string {
  return names[MODEL_KEYS[p]] || 'CLI default';
}

let provider = $state<Provider>('gemini');
// False until a provider is saved or picked. Until then (a new install) the
// store shows the first available provider and saves nothing.
let providerChosen = false;
let models = $state<ModelNames>({ gemini_model: '', claude_model: '', openai_model: '' });
// Which providers can answer; null until the backend has answered.
let availability = $state<Record<Provider, boolean> | null>(null);

// Same rule as the overlay: with nothing chosen, move off a provider that
// cannot answer to the first one that can.
function showFirstAvailable(): void {
  if (providerChosen || !availability || availability[provider]) return;
  const known = availability;
  const first = PROVIDER_ORDER.find((p) => known[p]);
  if (first) provider = first;
}

/** The model label for `p`, as last reported by the backend. */
export function getModelName(p: Provider): string {
  return modelName(models, p);
}

/** Which providers can answer, as last reported; null until known. */
export function getAvailability(): Record<Provider, boolean> | null {
  return availability;
}

/**
 * Re-read which providers can answer and the model each uses. CLI detection
 * finishes after startup, and Settings can change the Gemini model or key.
 */
export async function refreshAvailability(): Promise<void> {
  try {
    const a = await invoke<ModelNames & Record<Provider, boolean>>('available_providers');
    models = {
      gemini_model: a.gemini_model,
      claude_model: a.claude_model,
      openai_model: a.openai_model,
    };
    availability = { gemini: a.gemini, claude: a.claude, openai: a.openai };
    showFirstAvailable();
  } catch (err: unknown) {
    console.error('Failed to read provider availability:', err);
  }
}

export function getProvider(): Provider {
  return provider;
}

export function getProviderMeta(): ProviderMeta {
  return PROVIDERS[provider];
}

export function setProvider(p: Provider): void {
  provider = p;
  providerChosen = true;
  void invoke('set_active_provider', { provider: p }).catch(() => {
    /* selection still applies for this session */
  });
}

/**
 * Load the persisted provider on startup. An empty value (never chosen) or an
 * unknown one leaves the choice open, so the first available provider shows.
 */
export async function loadProvider(): Promise<void> {
  try {
    const settings = await invoke<{ active_provider?: string }>('get_settings');
    const saved = settings.active_provider;
    if (saved && saved in PROVIDERS) {
      provider = saved as Provider;
      providerChosen = true;
    }
  } catch {
    /* no saved choice */
  }
  showFirstAvailable();
}
