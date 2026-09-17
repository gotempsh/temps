// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Single source of truth for AI provider metadata + brand icons. Consumed
// by `/ai-gateway`, `AiQuickstart`, the agent-sandbox providers list, and
// anywhere else we need to render a provider identity consistently.
//
// Add a new provider by appending to `AI_PROVIDERS`. The icon lives in
// `AiProviderIcon`'s switch below. Keep the `id` string matched to the
// backend's provider enum (lowercase, no spaces).

export type AiProviderId =
  'openai' | 'anthropic' | 'xai' | 'gemini' | 'openrouter'

export interface AiProviderMeta {
  id: AiProviderId
  /** Display name shown to users. */
  name: string
  /** One-line model list for tooltips/help text. */
  models: string
  /** Canonical default model used in code snippets and sample requests. */
  defaultModel: string
  /**
   * Tailwind class applied to the icon container so the brand reads at a
   * glance. Kept deliberately muted (subtle tinted background + full-color
   * mark) — a saturated card header would fight every other status
   * indicator on the page.
   */
  accentClass: string
  /** Short tagline for cards that show the provider without models. */
  tagline: string
  /** Public docs URL for "Where do I find my key?" links. */
  keyDocsUrl: string
}

export const AI_PROVIDERS: readonly AiProviderMeta[] = [
  {
    id: 'openai',
    name: 'OpenAI',
    tagline: 'GPT-5.6 family and o-series reasoning models',
    models: 'GPT-5.6 Sol, GPT-5.6 Terra, GPT-5.6 Luna, GPT-5 Nano, o3',
    defaultModel: 'gpt-5-nano',
    accentClass: 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400',
    keyDocsUrl: 'https://platform.openai.com/api-keys',
  },
  {
    id: 'anthropic',
    name: 'Anthropic',
    tagline: 'Claude Opus, Sonnet, and Haiku',
    models: 'Claude Opus 5, Claude Sonnet 5, Claude Fable 5, Claude Haiku 4.5',
    defaultModel: 'claude-haiku-4-5',
    accentClass: 'bg-orange-500/10 text-orange-600 dark:text-orange-400',
    keyDocsUrl: 'https://console.anthropic.com/settings/keys',
  },
  {
    id: 'xai',
    name: 'xAI',
    tagline: 'Grok reasoning and code models',
    models: 'Grok 4.5, Grok 4.20',
    defaultModel: 'grok-4.5',
    accentClass:
      'bg-neutral-900/10 text-neutral-900 dark:bg-neutral-50/10 dark:text-neutral-50',
    keyDocsUrl: 'https://console.x.ai/team/default/api-keys',
  },
  {
    id: 'gemini',
    name: 'Google Gemini',
    tagline: 'Gemini Pro and Flash',
    models: 'Gemini 3.6 Flash, Gemini 3.5 Flash, Gemini 3.1 Pro',
    defaultModel: 'gemini-3.5-flash-lite',
    accentClass: 'bg-sky-500/10 text-sky-600 dark:text-sky-400',
    keyDocsUrl: 'https://aistudio.google.com/app/apikey',
  },
  {
    id: 'openrouter',
    name: 'OpenRouter',
    tagline: 'One key, hundreds of models from every vendor',
    models: 'GPT-4o, Claude Sonnet 5, Llama 3.3, DeepSeek, and more',
    defaultModel: 'openai/gpt-4o-mini',
    accentClass: 'bg-violet-500/10 text-violet-600 dark:text-violet-400',
    keyDocsUrl: 'https://openrouter.ai/keys',
  },
]

export const BY_ID: Record<string, AiProviderMeta> = Object.fromEntries(
  AI_PROVIDERS.map((p) => [p.id, p])
)

/** Look up provider metadata by id. Returns `undefined` for unknown ids. */
export function getAiProvider(id: string): AiProviderMeta | undefined {
  return BY_ID[id]
}

/** Human-readable name for a provider id. Falls back to the id itself. */
export function aiProviderName(id: string): string {
  return BY_ID[id]?.name ?? id
}

/** Comma-separated model list for a provider id. Falls back to empty string. */
export function aiProviderModels(id: string): string {
  return BY_ID[id]?.models ?? ''
}

// ──────────────────────────────────────────────────────────────────────────
// Icons
//
// Inline SVGs keyed by provider id. Each mark is the official brand glyph
// rendered with `fill="currentColor"` so it picks up whatever text color
// the surrounding element sets — works in both light and dark themes and
// lets callers recolor via Tailwind without touching the SVG.
// ──────────────────────────────────────────────────────────────────────────

export interface IconBaseProps {
  className?: string
  width?: number
  height?: number
}

export interface AiProviderIconProps extends IconBaseProps {
  provider: string
  /**
   * When `true` (default), wraps the mark in a tinted rounded square using
   * the provider's `accentClass`. Set to `false` to render just the glyph
   * — useful for tight inline spots like select items.
   */
  tinted?: boolean
  /**
   * Size of the rounded wrapper when `tinted`. Defaults to 10 (= h-10 w-10).
   * The inner mark is sized to ~60 % of the wrapper.
   */
  size?: number
}
