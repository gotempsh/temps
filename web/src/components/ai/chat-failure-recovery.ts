// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function canRefreshLocalCredential(code: string, provider: string) {
  // Claude requires an explicit setup token. Codex and native OpenCode
  // support importing the server's local credential store.
  return code === 'harness_authentication_required' &&
    (provider === 'codex' || provider === 'opencode')
}

export function isHarnessFailure(code: string) {
  return code.startsWith('harness_') || code.startsWith('provider_') ||
    code === 'empty_provider_response' || code === 'model_unavailable' ||
    code === 'unsupported_workspace_credential' || code === 'invalid_workspace_credential'
}
