// Single source of truth for AI provider metadata + brand icons. Consumed
// by `/ai-gateway`, `AiQuickstart`, the agent-sandbox providers list, and
// anywhere else we need to render a provider identity consistently.
//
// Add a new provider by appending to `AI_PROVIDERS`. The icon lives in
// `AiProviderIcon`'s switch below. Keep the `id` string matched to the
// backend's provider enum (lowercase, no spaces).

export type AiProviderId = 'openai' | 'anthropic' | 'xai' | 'gemini'

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
]

const BY_ID: Record<string, AiProviderMeta> = Object.fromEntries(
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

interface IconBaseProps {
  className?: string
  width?: number
  height?: number
}

function OpenAIMark(props: IconBaseProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
      {...props}
    >
      <path d="M22.282 9.821a5.985 5.985 0 0 0-.516-4.91 6.046 6.046 0 0 0-6.51-2.9A6.065 6.065 0 0 0 4.981 4.18a5.985 5.985 0 0 0-3.998 2.9 6.046 6.046 0 0 0 .743 7.097 5.98 5.98 0 0 0 .51 4.911 6.051 6.051 0 0 0 6.515 2.9A5.985 5.985 0 0 0 13.26 24a6.056 6.056 0 0 0 5.772-4.206 5.99 5.99 0 0 0 3.997-2.9 6.056 6.056 0 0 0-.747-7.073zM13.26 22.43a4.476 4.476 0 0 1-2.876-1.04l.141-.081 4.779-2.758a.795.795 0 0 0 .392-.681v-6.737l2.02 1.168a.071.071 0 0 1 .038.052v5.583a4.504 4.504 0 0 1-4.494 4.494zM3.6 18.304a4.47 4.47 0 0 1-.535-3.014l.142.085 4.783 2.759a.771.771 0 0 0 .78 0l5.843-3.369v2.332a.08.08 0 0 1-.033.062L9.74 19.95a4.5 4.5 0 0 1-6.14-1.646zM2.34 7.896a4.485 4.485 0 0 1 2.366-1.973V11.6a.766.766 0 0 0 .388.676l5.815 3.355-2.02 1.168a.076.076 0 0 1-.071 0l-4.83-2.786A4.504 4.504 0 0 1 2.34 7.872zm16.597 3.855l-5.833-3.387L15.119 7.2a.076.076 0 0 1 .071 0l4.83 2.791a4.494 4.494 0 0 1-.676 8.105v-5.678a.79.79 0 0 0-.407-.667zm2.01-3.023l-.141-.085-4.774-2.782a.776.776 0 0 0-.785 0L9.409 9.23V6.897a.066.066 0 0 1 .028-.061l4.83-2.787a4.5 4.5 0 0 1 6.68 4.66zm-12.64 4.135l-2.02-1.164a.08.08 0 0 1-.038-.057V6.075a4.5 4.5 0 0 1 7.375-3.453l-.142.08L8.704 5.46a.795.795 0 0 0-.393.681zm1.097-2.365l2.602-1.5 2.607 1.5v3l-2.597 1.5-2.607-1.5z" />
    </svg>
  )
}

function AnthropicMark(props: IconBaseProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
      {...props}
    >
      <path d="M17.304 3.541h-3.672l6.696 16.918H24Zm-10.608 0L0 20.459h3.744l1.37-3.553h7.005l1.369 3.553h3.744L10.536 3.541Zm-.371 10.223 2.293-5.945 2.293 5.945Z" />
    </svg>
  )
}

function XAIMark(props: IconBaseProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
      {...props}
    >
      <path d="M3.005 8.858l8.783 12.544h3.904L6.908 8.858zM6.905 15.825L3 21.402h3.907l1.951-2.788zM16.585 2l-6.75 9.64 1.953 2.79L20.492 2zM17.292 7.965v13.437h3.2V3.395z" />
    </svg>
  )
}

function GeminiMark(props: IconBaseProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
      {...props}
    >
      <path d="M12 24A14.304 14.304 0 0 0 0 12 14.304 14.304 0 0 0 12 0a14.305 14.305 0 0 0 12 12 14.305 14.305 0 0 0-12 12Z" />
    </svg>
  )
}

function GenericProviderMark(props: IconBaseProps) {
  // Fallback for provider ids we don't have a branded glyph for yet — a
  // neutral diamond so we never render a broken surface.
  return (
    <svg
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
      {...props}
    >
      <path d="M12 2 2 12l10 10 10-10Zm0 4.243L17.757 12 12 17.757 6.243 12Z" />
    </svg>
  )
}

interface AiProviderIconProps extends IconBaseProps {
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

/**
 * Reusable provider icon. Dispatches to the right brand mark by id; falls
 * back to a generic diamond for unknown providers.
 *
 * ```tsx
 * <AiProviderIcon provider="openai" />           // tinted square, 40x40
 * <AiProviderIcon provider="openai" size={28} /> // tinted square, 28x28
 * <AiProviderIcon provider="openai" tinted={false} width={16} height={16} />
 * ```
 */
export function AiProviderIcon({
  provider,
  tinted = true,
  size = 40,
  className = '',
  width,
  height,
}: AiProviderIconProps) {
  const meta = getAiProvider(provider)
  const Mark =
    provider === 'openai'
      ? OpenAIMark
      : provider === 'anthropic'
        ? AnthropicMark
        : provider === 'xai'
          ? XAIMark
          : provider === 'gemini'
            ? GeminiMark
            : GenericProviderMark

  if (!tinted) {
    return (
      <Mark
        className={className}
        width={width ?? size}
        height={height ?? size}
      />
    )
  }

  const inner = Math.round(size * 0.55)
  return (
    <div
      className={`inline-flex items-center justify-center rounded-lg shrink-0 ${
        meta?.accentClass ?? 'bg-muted text-muted-foreground'
      } ${className}`}
      style={{ width: size, height: size }}
      aria-hidden="true"
    >
      <Mark width={inner} height={inner} />
    </div>
  )
}
