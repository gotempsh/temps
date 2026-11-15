// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { AssistantModelOption } from './assistant-models'

/** Who makes a model, as far as its id says. */
export interface ModelVendor {
  /** Stable key: the lowercase vendor slug, `other` when the id names none. */
  key: string
  /** Name to show. */
  name: string
  /** File in `public/ai-agents/`, when the repository ships that vendor's mark. */
  logo: string | null
}

interface KnownVendor {
  name: string
  logo?: string
}

/**
 * Vendor slugs as they appear before the `/` in a model id. Logos reuse the
 * marks already shipped for the AI-agent analytics; a vendor without one
 * renders as a lettered chip instead of a missing image.
 */
const KNOWN_VENDORS: Record<string, KnownVendor> = {
  openai: { name: 'OpenAI', logo: 'openai.svg' },
  anthropic: { name: 'Anthropic', logo: 'anthropic.svg' },
  google: { name: 'Google', logo: 'google.svg' },
  mistralai: { name: 'Mistral', logo: 'mistral.svg' },
  deepseek: { name: 'DeepSeek', logo: 'deepseek.svg' },
  'x-ai': { name: 'xAI', logo: 'xai.svg' },
  meta: { name: 'Meta', logo: 'meta.svg' },
  'meta-llama': { name: 'Meta', logo: 'meta.svg' },
  cohere: { name: 'Cohere', logo: 'cohere.svg' },
  amazon: { name: 'Amazon', logo: 'amazon.svg' },
  perplexity: { name: 'Perplexity', logo: 'perplexity.svg' },
  bytedance: { name: 'ByteDance', logo: 'bytedance.svg' },
  'bytedance-seed': { name: 'ByteDance', logo: 'bytedance.svg' },
  openrouter: { name: 'OpenRouter', logo: 'openrouter.svg' },
  apple: { name: 'Apple', logo: 'apple.svg' },
  qwen: { name: 'Qwen' },
  'z-ai': { name: 'Z.ai' },
  nvidia: { name: 'NVIDIA' },
  moonshotai: { name: 'Moonshot AI' },
  minimax: { name: 'MiniMax' },
  microsoft: { name: 'Microsoft' },
  tencent: { name: 'Tencent' },
  xiaomi: { name: 'Xiaomi' },
}

/** Name of the vendor given to ids that name none. */
export const OTHER_VENDOR_NAME = 'Other'

const OTHER: ModelVendor = { key: 'other', name: OTHER_VENDOR_NAME, logo: null }

/** Ids with no `vendor/` prefix still name their family. */
function vendorFromBareId(id: string): string | null {
  const lower = id.toLowerCase()
  if (lower.startsWith('claude')) return 'anthropic'
  if (lower.startsWith('gemini') || lower.startsWith('gemma')) return 'google'
  if (/^(gpt|chatgpt|o\d)/.test(lower)) return 'openai'
  if (lower.startsWith('deepseek')) return 'deepseek'
  if (lower.startsWith('grok')) return 'x-ai'
  if (lower.startsWith('mistral') || lower.startsWith('codestral')) {
    return 'mistralai'
  }
  return null
}

function titleCase(slug: string): string {
  return slug
    .split(/[-_.]/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ')
}

/**
 * The vendor of a model id: `google/gemini-3.5-flash` is Google. A leading `~`
 * (a router alias such as `~openai/gpt-latest`) belongs to the same vendor.
 */
export function modelVendor(modelId: string): ModelVendor {
  const id = modelId.trim().replace(/^~/, '')
  const slash = id.indexOf('/')
  const slug = (slash > 0 ? id.slice(0, slash) : vendorFromBareId(id))
    ?.toLowerCase()
    .trim()
  if (!slug) return OTHER
  const known = KNOWN_VENDORS[slug]
  if (known) {
    // Two slugs can share a vendor (`meta` and `meta-llama`): key by name.
    return {
      key: known.name.toLowerCase().replace(/[^a-z0-9]+/g, '-'),
      name: known.name,
      logo: known.logo ?? null,
    }
  }
  return { key: slug, name: titleCase(slug) || slug, logo: null }
}

/**
 * The vendors people reach for first, in order. Tabs and the model list follow
 * it, then everyone else by how many models they list. The catalogue carries no
 * popularity or release date, so this is an editorial list: reorder it here.
 */
const VENDOR_RANK = [
  'anthropic',
  'openai',
  'google',
  'xai',
  'deepseek',
  'mistral',
  'meta',
  'qwen',
] as const

/** Position of a vendor in [`VENDOR_RANK`]; unranked vendors sort after it. */
export function vendorRank(key: string): number {
  const index = (VENDOR_RANK as readonly string[]).indexOf(key)
  return index === -1 ? VENDOR_RANK.length : index
}

/** A vendor the list can be narrowed to, and how many listed models are theirs. */
export interface VendorOption {
  key: string
  name: string
  logo: string | null
  count: number
}

/**
 * Vendors for the picker's filter: the best-known first, then the biggest.
 * Every vendor is listed; a dropdown holds the long tail that a row of tabs
 * could not.
 */
export function vendorOptions(
  options: readonly AssistantModelOption[]
): VendorOption[] {
  const byKey = new Map<string, VendorOption>()
  for (const option of options) {
    const vendor = modelVendor(option.id)
    const entry = byKey.get(vendor.key)
    if (entry) entry.count += 1
    else byKey.set(vendor.key, { ...vendor, count: 1 })
  }
  return [...byKey.values()].sort(
    (left, right) =>
      vendorRank(left.key) - vendorRank(right.key) ||
      right.count - left.count ||
      left.name.localeCompare(right.name)
  )
}

/** The options of one vendor; every option when `vendorKey` is `null`. */
export function filterByVendor(
  options: readonly AssistantModelOption[],
  vendorKey: string | null
): AssistantModelOption[] {
  if (vendorKey === null) return [...options]
  return options.filter((option) => modelVendor(option.id).key === vendorKey)
}

function squash(text: string): string {
  return text.toLowerCase().replace(/[^a-z0-9]+/g, '')
}

/**
 * The model's name without the vendor's name in front of it. Catalogues title
 * models "Anthropic: Claude Opus 5.5"; beside the vendor's logo the prefix says
 * the same thing twice and costs a third of a phone-width row.
 */
export function shortModelName(name: string, modelId: string): string {
  const colon = name.indexOf(': ')
  if (colon <= 0) return name
  const prefix = squash(name.slice(0, colon))
  const slug = squash(modelId.replace(/^~/, '').split('/')[0] ?? '')
  const vendor = squash(modelVendor(modelId).name)
  const rest = name.slice(colon + 2).trim()
  if (rest && (prefix === vendor || prefix === slug)) return rest
  return name
}

const naturalOrder = new Intl.Collator('en', { numeric: true })

/** Newest-looking first: higher version numbers sort ahead of lower ones. */
function newestFirst(left: string, right: string): number {
  return naturalOrder.compare(right, left)
}

/**
 * Model families worth showing before the catalogue's several hundred entries.
 * Each pattern names a family, not a version: the newest match is chosen, so a
 * release does not need an edit here.
 */
const FEATURED_FAMILIES: readonly RegExp[] = [
  /^anthropic\/claude-opus/,
  /^anthropic\/claude-sonnet/,
  /^anthropic\/claude-haiku/,
  /^openai\/gpt-[\d.]+$/,
  /^openai\/gpt-[\d.]+-mini$/,
  /^google\/gemini-[\d.]+-pro$/,
  /^google\/gemini-[\d.]+-flash$/,
  /^x-ai\/grok-[\d.]+$/,
  /^deepseek\/deepseek-/,
  /^mistralai\/mistral-large/,
  /^qwen\/qwen[\d.]*-(max|plus)/,
]

/** Variants that are previews, aliases or specialised, not a family's flagship. */
function isFlagshipCandidate(id: string): boolean {
  return !/[:~]|preview|exp(erimental)?\b|latest|-(thinking|search|image|audio|tts|codex|oss|lite)\b/.test(
    id
  )
}

/**
 * The models to pin at the top: the newest of each featured family, only among
 * those the provider actually lists and that can call tools.
 */
export function featuredModels(
  options: readonly AssistantModelOption[]
): AssistantModelOption[] {
  const usable = options.filter(
    (option) =>
      isFlagshipCandidate(option.id) && option.details.supports_tools !== false
  )
  const featured: AssistantModelOption[] = []
  for (const family of FEATURED_FAMILIES) {
    const match = usable
      .filter((option) => family.test(option.id))
      .sort((left, right) => newestFirst(left.id, right.id))[0]
    if (match && !featured.includes(match)) featured.push(match)
  }
  return featured
}

/**
 * Catalogue order for browsing: best-known vendors first, newest version first
 * within a vendor. Alphabetical put "AI21" ahead of every model anyone uses.
 */
export function orderByPopularity(
  options: readonly AssistantModelOption[]
): AssistantModelOption[] {
  return [...options].sort((left, right) => {
    const a = modelVendor(left.id)
    const b = modelVendor(right.id)
    return (
      vendorRank(a.key) - vendorRank(b.key) ||
      a.name.localeCompare(b.name) ||
      newestFirst(left.id, right.id)
    )
  })
}
