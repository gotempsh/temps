// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Capabilities declared on a self-hosted (OpenAI-compatible) provider key. */
export interface SelfHostedCapabilitiesInput {
  streaming: boolean
  tools: boolean
  embeddings: boolean
  responses_api: boolean
}

/**
 * What a new key declares: everything. The declaration only gates requests
 * (Temps refuses one the key does not declare), so a server that lacks an
 * endpoint answers with its own error; narrowing it is an advanced option.
 */
export const DEFAULT_SELF_HOSTED_CAPABILITIES: SelfHostedCapabilitiesInput = {
  streaming: true,
  tools: true,
  embeddings: true,
  responses_api: true,
}

/**
 * Mirrors the server defaults for a stored declaration that omits a
 * capability: chat, streaming and tools only.
 */
export const SERVER_DEFAULT_SELF_HOSTED_CAPABILITIES: SelfHostedCapabilitiesInput =
  {
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
  const defaults = SERVER_DEFAULT_SELF_HOSTED_CAPABILITIES
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

/**
 * Whether a base URL is complete enough to ask the server for its models: an
 * http(s) URL with a host. This only rules out URLs that cannot be requested;
 * a half-typed URL can still name a real host (`https://api.example.co` on
 * the way to `.com`), so discovery never sends an API key on its own.
 */
export function isDiscoverableBaseUrl(url: string): boolean {
  const trimmed = url.trim()
  if (!/^https?:\/\//i.test(trimmed)) return false
  try {
    const host = new URL(trimmed).hostname
    return host !== '' && !host.endsWith('.')
  } catch {
    return false
  }
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

function isPrivateIpv4(octets: number[]): boolean {
  const [a, b] = octets
  return (
    a === 10 ||
    a === 127 ||
    a === 0 ||
    (a === 172 && b >= 16 && b <= 31) ||
    (a === 192 && b === 168) ||
    (a === 169 && b === 254) ||
    (a === 100 && b >= 64 && b <= 127)
  )
}

function isPrivateIpv6(address: string): boolean {
  if (address === '::1' || address === '::') return true
  // IPv4-mapped, which URL parsing rewrites to hex (`::ffff:a00:5`).
  const mapped = address.match(/^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/)
  if (mapped) {
    const high = parseInt(mapped[1], 16)
    const low = parseInt(mapped[2], 16)
    return isPrivateIpv4([high >> 8, high & 0xff, low >> 8, low & 0xff])
  }
  // Unique local (fc00::/7) and link-local (fe80::/10).
  return /^f[cd]/.test(address) || /^fe[89ab]/.test(address)
}

/**
 * Whether the base URL points at a private address: a private, loopback,
 * link-local or carrier-grade NAT IP, or a name that only resolves on a
 * local network (`localhost`, a single-label Docker service name, `.local`,
 * `.internal`, `.lan`, `.home.arpa`). Only such a URL needs, or can use,
 * private address trust.
 */
export function isPrivateAddressUrl(url: string): boolean {
  let host: string
  try {
    host = new URL(url.trim()).hostname.toLowerCase()
  } catch {
    return false
  }
  if (!host) return false
  if (host.startsWith('[') && host.endsWith(']')) {
    return isPrivateIpv6(host.slice(1, -1))
  }
  const ipv4 = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/)
  if (ipv4) return isPrivateIpv4(ipv4.slice(1).map(Number))
  if (!host.includes('.')) return true
  return /\.(localhost|local|internal|lan|home\.arpa)$/.test(host)
}

/** Why the self-hosted form cannot be submitted yet, or null when it can. */
export function selfHostedSubmitBlocker(input: {
  baseUrl: string
  discoveredModels: string[] | null
  defaultModel: string
}): string | null {
  if (!input.baseUrl.trim()) return 'Enter the base URL of your server.'
  if (input.discoveredModels === null) {
    return 'Temps has not listed the models your server serves yet. Check the base URL and API key.'
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
