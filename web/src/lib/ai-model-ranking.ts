// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

const NON_TEXT_MODEL_MARKERS = [
  'audio',
  'dall-e',
  'embedding',
  'image',
  'moderation',
  'realtime',
  'search',
  'transcribe',
  'tts',
] as const

/**
 * Rank generally useful, current text models ahead of legacy and
 * special-purpose models. Provider discovery APIs commonly return model IDs
 * alphabetically, which puts GPT-3.5 before GPT-5.6 and image models before
 * chat models. This score is deliberately provider-neutral and deterministic.
 */
export function aiModelRelevanceScore(modelId: string): number {
  const id = modelId.toLowerCase()
  let score = 0

  const version = id.match(
    /(?:gpt|gemini|grok)[-_]?(\d+)(?:[.-](\d+))?|claude-(?:opus|sonnet|haiku|fable)-?(\d+)(?:[.-](\d+))?/
  )
  const major = Number(version?.[1] ?? version?.[3] ?? 0)
  const minor = Number(version?.[2] ?? version?.[4] ?? 0)
  score += major * 10_000 + minor * 100

  if (id.includes('latest')) score += 40
  if (id.includes('sol')) score += 30
  if (id.includes('terra')) score += 20
  if (id.includes('luna')) score += 10
  if (id.includes('preview')) score -= 20
  if (id.includes('turbo')) score -= 50
  if (id.includes('mini')) score -= 10
  if (id.includes('nano')) score -= 20
  if (/-20\d{2}(?:-|$)/.test(id)) score -= 5
  if (NON_TEXT_MODEL_MARKERS.some((marker) => id.includes(marker))) {
    score -= 100_000
  }

  return score
}

export function compareAiModelIdsByRelevance(a: string, b: string): number {
  const scoreDifference = aiModelRelevanceScore(b) - aiModelRelevanceScore(a)
  return scoreDifference || a.localeCompare(b)
}

export function sortAiModelIdsByRelevance(modelIds: string[]): string[] {
  return [...modelIds].sort(compareAiModelIdsByRelevance)
}
