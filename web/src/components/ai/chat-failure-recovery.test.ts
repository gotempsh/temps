// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { describe, expect, test } from 'bun:test'
import { canRefreshLocalCredential, isHarnessFailure } from './chat-failure-recovery'

describe('chat failure recovery', () => {
  test('offers supported local refresh only for authentication failures', () => {
    expect(canRefreshLocalCredential('harness_authentication_required', 'codex')).toBe(true)
    expect(canRefreshLocalCredential('harness_authentication_required', 'opencode')).toBe(true)
    for (const provider of ['claude_cli', 'gateway']) {
      expect(canRefreshLocalCredential('harness_authentication_required', provider)).toBe(false)
    }
    for (const code of ['provider_rate_limited', 'provider_quota_exhausted', 'unsupported_workspace_credential']) {
      expect(canRefreshLocalCredential(code, 'codex')).toBe(false)
    }
  })
  test('does not resend chat for attachment or conversation errors', () => {
    expect(isHarnessFailure('attachment_limit')).toBe(false)
    expect(isHarnessFailure('turn_in_progress')).toBe(false)
    expect(isHarnessFailure('provider_rate_limited')).toBe(true)
    expect(isHarnessFailure('empty_provider_response')).toBe(true)
  })
})
