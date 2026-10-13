// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Capabilities declared on a self-hosted (OpenAI-compatible) provider key. */
export interface SelfHostedCapabilitiesInput {
  streaming: boolean
  tools: boolean
  embeddings: boolean
  responses_api: boolean
}

/** Mirrors the server defaults: chat, streaming and tools only. */
export const DEFAULT_SELF_HOSTED_CAPABILITIES: SelfHostedCapabilitiesInput = {
  streaming: true,
  tools: true,
  embeddings: false,
  responses_api: false,
}

/** Fill in server defaults for any capability a stored declaration omits. */
export function normalizeSelfHostedCapabilities(
  declared:
    | Partial<Record<keyof SelfHostedCapabilitiesInput, boolean | null>>
    | null
    | undefined
): SelfHostedCapabilitiesInput {
  const defaults = DEFAULT_SELF_HOSTED_CAPABILITIES
  return {
    streaming: declared?.streaming ?? defaults.streaming,
    tools: declared?.tools ?? defaults.tools,
    embeddings: declared?.embeddings ?? defaults.embeddings,
    responses_api: declared?.responses_api ?? defaults.responses_api,
  }
}

export const SELF_HOSTED_CAPABILITY_LABELS: Record<
  keyof SelfHostedCapabilitiesInput,
  { label: string; description: string }
> = {
  streaming: {
    label: 'Streaming',
    description: 'Streamed chat completions. AI chat needs this.',
  },
  tools: {
    label: 'Tool calling',
    description: 'Function/tool calls in chat completions.',
  },
  embeddings: {
    label: 'Embeddings',
    description: 'The /embeddings endpoint.',
  },
  responses_api: {
    label: 'Responses API',
    description: 'The /responses endpoint.',
  },
}

/**
 * Identifies the inputs a model discovery ran with. A discovered list only
 * applies while the base URL, key and trust choice are unchanged.
 */
export function selfHostedDiscoverySignature(input: {
  baseUrl: string
  apiKey: string
  trustPrivateAddress: boolean
}): string {
  return JSON.stringify([
    input.baseUrl.trim(),
    input.apiKey,
    input.trustPrivateAddress,
  ])
}

/** Whether a base URL uses plain, unencrypted http://. */
export function isPlainHttpUrl(url: string): boolean {
  return /^http:\/\//i.test(url.trim())
}

/**
 * Whether the base URL's host is an IP address literal. Only such a URL can
 * be trusted on a single provider key; a hostname may resolve elsewhere later.
 */
export function hasIpLiteralHost(url: string): boolean {
  try {
    const host = new URL(url.trim()).hostname
    return /^\d{1,3}(\.\d{1,3}){3}$/.test(host) || host.startsWith('[')
  } catch {
    return false
  }
}

/** Why the self-hosted form cannot be submitted yet, or null when it can. */
export function selfHostedSubmitBlocker(input: {
  baseUrl: string
  discoveredModels: string[] | null
  defaultModel: string
}): string | null {
  if (!input.baseUrl.trim()) return 'Enter the base URL of your server.'
  if (input.discoveredModels === null) {
    return 'Load the models your server lists, then pick a default model.'
  }
  if (input.discoveredModels.length === 0) {
    return 'Your server lists no models at /models. Load a model on it first.'
  }
  if (!input.discoveredModels.includes(input.defaultModel)) {
    return 'Pick a default model from the list. It is used to verify the connection.'
  }
  return null
}

/**
 * React Query mutation key of provider key settings saves (trust,
 * capabilities). Which keys are saving is read back from the mutation cache
 * with `useMutationState`, never kept in component state.
 */
export const PROVIDER_KEY_SETTINGS_MUTATION_KEY = [
  'providerKeySettings',
] as const

/** Variables of a provider key settings save. */
export interface ProviderKeySettingsVariables {
  id: number
}

/** The key id a pending settings save is for. */
export function settingsSaveKeyId(variables: unknown): number | undefined {
  const id = (variables as Partial<ProviderKeySettingsVariables> | undefined)
    ?.id
  return typeof id === 'number' ? id : undefined
}
