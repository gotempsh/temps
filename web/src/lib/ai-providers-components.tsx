// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getAiProvider,
  type IconBaseProps,
  type AiProviderIconProps,
} from './ai-providers-shared'

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
  const asset = ['openrouter', 'mistral', 'deepseek'].includes(provider)
    ? `/ai-agents/${provider}.svg`
    : undefined
  if (asset) {
    const mark = (
      <img
        src={asset}
        alt=""
        aria-hidden="true"
        width={tinted ? Math.round(size * 0.55) : (width ?? size)}
        height={tinted ? Math.round(size * 0.55) : (height ?? size)}
        className={`shrink-0 dark:invert ${tinted ? '' : className}`}
      />
    )
    return tinted ? (
      <span
        aria-hidden="true"
        className={`inline-flex shrink-0 items-center justify-center rounded-lg ${meta?.accentClass ?? 'bg-muted'} ${className}`}
        style={{ width: size, height: size }}
      >
        {mark}
      </span>
    ) : (
      mark
    )
  }
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
