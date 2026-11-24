// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function canRefreshLocalCredential(code: string, provider: string) {
  // Claude requires an explicit setup token. Codex and native OpenCode
  // support importing the server's local credential store.
  return (
    code === 'harness_authentication_required' &&
    (provider === 'codex' || provider === 'opencode')
  )
}

export function isHarnessFailure(code: string) {
  return (
    code.startsWith('harness_') ||
    code.startsWith('provider_') ||
    code === 'empty_provider_response' ||
    code === 'model_unavailable' ||
    code === 'unsupported_workspace_credential' ||
    code === 'invalid_workspace_credential'
  )
}

/**
 * Where a failed chat's provider is configured. A chat on the AI gateway
 * (`gateway`, or one gateway key as `gateway_key:<id>`) is set up under AI
 * Gateway; only a host harness has a page of its own under Harnesses.
 */
export function chatProviderSettingsHref(provider: string) {
  if (isGatewayChatProvider(provider)) return '/ai-gateway'
  return `/agent-sandbox/providers/${encodeURIComponent(provider)}`
}

/** Where to pick a different provider after this one ran out of quota. */
export function chatProvidersHref(provider: string) {
  return isGatewayChatProvider(provider)
    ? '/ai-gateway'
    : '/agent-sandbox/providers'
}

function isGatewayChatProvider(provider: string) {
  return provider === 'gateway' || provider.startsWith('gateway_key:')
}
