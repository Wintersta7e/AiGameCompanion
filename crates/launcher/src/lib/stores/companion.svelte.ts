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
let models = $state<ModelNames>({ gemini_model: '', claude_model: '', openai_model: '' });

/** The model label for `p`, as last reported by the backend. */
export function getModelName(p: Provider): string {
  return modelName(models, p);
}

/** Re-read the model names, e.g. after Settings saved a new Gemini model. */
export async function refreshModels(): Promise<void> {
  try {
    models = await invoke<ModelNames>('available_providers');
  } catch {
    /* keep the last known names */
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
  void invoke('set_active_provider', { provider: p }).catch(() => {
    /* selection still applies for this session */
  });
}

/** Load the persisted provider on startup. */
export async function loadProvider(): Promise<void> {
  try {
    const settings = await invoke<{ active_provider?: string }>('get_settings');
    const saved = settings.active_provider;
    if (saved && saved in PROVIDERS) provider = saved as Provider;
  } catch {
    /* keep the default */
  }
}
