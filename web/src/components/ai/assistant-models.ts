// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** What a provider catalogue says about a model. Every field may be absent. */
export interface AssistantModelDetails {
  name?: string | null
  context_length?: number | null
  input_modalities?: string[] | null
  output_modalities?: string[] | null
  /** USD per one million input tokens. */
  input_price_per_million?: number | null
  /** USD per one million output tokens. */
  output_price_per_million?: number | null
  supports_tools?: boolean | null
}

/** The slice of a provider model row the picker needs. */
export interface AssistantModelRow {
  model_id: string
  display_name: string
  is_available: boolean
  is_enabled: boolean
  details?: AssistantModelDetails | null
}

export interface AssistantModelOption {
  id: string
  name: string
  details: AssistantModelDetails
}

/** Models a conversation can actually be started on, best-known name first. */
export function assistantModelOptions(
  rows: readonly AssistantModelRow[] | undefined
): AssistantModelOption[] {
  return (
    (rows ?? [])
      .filter((row) => row.is_available && row.is_enabled)
      // Embedding models cannot chat.
      .filter((row) => !row.model_id.startsWith('text-embedding-'))
      // A `:batch` variant is the provider's asynchronous batch endpoint: the
      // gateway answers a chat request on it with a 404, so it can never be
      // a chat's model.
      .filter((row) => !row.model_id.endsWith(':batch'))
      .map((row) => ({
        id: row.model_id,
        name: row.details?.name?.trim() || row.display_name || row.model_id,
        details: row.details ?? {},
      }))
      .sort((left, right) => left.name.localeCompare(right.name))
  )
}

/** Case-insensitive match on the model's name or id. Empty query keeps all. */
export function filterAssistantModels(
  options: readonly AssistantModelOption[],
  query: string
): AssistantModelOption[] {
  const needle = query.trim().toLowerCase()
  if (!needle) return [...options]
  return options.filter(
    (option) =>
      option.name.toLowerCase().includes(needle) ||
      option.id.toLowerCase().includes(needle)
  )
}

/**
 * The assistant works by calling tools. A catalogue that says a model has no
 * tool calling means the chat would answer but never act, so the picker warns.
 * Unknown (`null`/`undefined`) is not a warning: many catalogues omit it.
 */
export function lacksToolCalling(option: AssistantModelOption): boolean {
  return option.details.supports_tools === false
}

/**
 * Whether the catalogue says a model reads images. `unknown` is a catalogue
 * that lists no input modalities; the model is given the benefit of the doubt
 * and the server has the final say.
 */
export type ImageSupport = 'yes' | 'no' | 'unknown'

export function imageSupport(
  details: AssistantModelDetails | null | undefined
): ImageSupport {
  const modalities = details?.input_modalities ?? []
  if (modalities.length === 0) return 'unknown'
  return modalities.some((modality) => modality.toLowerCase() === 'image')
    ? 'yes'
    : 'no'
}

const MODALITY_LABELS: Record<string, string> = {
  text: 'Text',
  image: 'Image',
  file: 'File',
  audio: 'Audio',
  video: 'Video',
}

/** `["text","image"]` -> `Text, Image`. Empty when the catalogue is silent. */
export function formatModalities(
  modalities: readonly string[] | null | undefined
): string {
  return (modalities ?? [])
    .map((modality) => MODALITY_LABELS[modality] ?? modality)
    .join(', ')
}

/** `200000` -> `200K`, `1048576` -> `1M`. */
export function formatContextLength(
  tokens: number | null | undefined
): string | null {
  if (tokens == null || !Number.isFinite(tokens) || tokens <= 0) return null
  if (tokens >= 1_000_000) {
    return `${trimTrailingZero(tokens / 1_000_000)}M`
  }
  if (tokens >= 1_000) return `${trimTrailingZero(tokens / 1_000)}K`
  return String(tokens)
}

function trimTrailingZero(value: number): string {
  return value.toFixed(1).replace(/\.0$/, '')
}

/** USD per million tokens: `$3`, `$0.25`, `Free`. Null when unknown. */
export function formatPricePerMillion(
  price: number | null | undefined
): string | null {
  if (price == null || !Number.isFinite(price) || price < 0) return null
  if (price === 0) return 'Free'
  if (price >= 100) return `$${Math.round(price)}`
  const text = price >= 1 ? price.toFixed(2) : price.toPrecision(2)
  return `$${text.replace(/\.?0+$/, '')}`
}

/**
 * One line for the picker's second row: what goes in and out, and what it
 * costs. Parts the catalogue does not publish are left out, never guessed.
 */
export function describeModel(option: AssistantModelOption): string[] {
  const { details } = option
  const parts: string[] = []
  const input = formatModalities(details.input_modalities)
  const output = formatModalities(details.output_modalities)
  if (input) parts.push(`In: ${input}`)
  if (output) parts.push(`Out: ${output}`)
  const context = formatContextLength(details.context_length)
  if (context) parts.push(`${context} context`)
  const inputPrice = formatPricePerMillion(details.input_price_per_million)
  const outputPrice = formatPricePerMillion(details.output_price_per_million)
  if (inputPrice && outputPrice) {
    parts.push(`${inputPrice} in · ${outputPrice} out per 1M tokens`)
  }
  return parts
}

/** Name to show for a model id, falling back to the id itself. */
export function modelDisplayName(
  options: readonly AssistantModelOption[],
  modelId: string | null | undefined
): string | null {
  if (!modelId) return null
  return options.find((option) => option.id === modelId)?.name ?? modelId
}
