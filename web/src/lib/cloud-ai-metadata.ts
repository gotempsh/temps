// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Exact-key opt-in for AI identification and usage; excludes conversation content. */
export const CLOUD_AI_METADATA_KEYS = [
  'gen_ai.provider.name',
  'gen_ai.system',
  'gen_ai.operation.name',
  'gen_ai.request.model',
  'gen_ai.response.model',
  'gen_ai.usage.input_tokens',
  'gen_ai.usage.output_tokens',
  'gen_ai.usage.prompt_tokens',
  'gen_ai.usage.completion_tokens',
  'gen_ai.usage.cache_creation.input_tokens',
  'gen_ai.usage.cache_read.input_tokens',
] as const

export function withCloudAiMetadata(
  keys: string[],
  enabled: boolean
): string[] {
  const metadata = new Set<string>(CLOUD_AI_METADATA_KEYS)
  return enabled
    ? [...new Set([...keys, ...metadata])]
    : keys.filter((key) => !metadata.has(key))
}

export function cloudAiMetadataMissing(settings: {
  write_mode: string
  fidelity: string
  attribute_allowlist: string[]
}): boolean {
  return (
    settings.write_mode === 'cloud' &&
    (settings.fidelity !== 'queryable' ||
      !CLOUD_AI_METADATA_KEYS.every((key) =>
        settings.attribute_allowlist.includes(key)
      ))
  )
}
