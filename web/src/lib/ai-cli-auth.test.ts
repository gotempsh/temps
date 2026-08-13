import { describe, expect, test } from 'bun:test'

import { hostAuthLabel } from './ai-cli-auth'

describe('hostAuthLabel', () => {
  test('identifies host-discovered Codex and OpenCode authentication', () => {
    expect(hostAuthLabel('chatgpt_subscription')).toBe(
      'Host environment · ChatGPT subscription'
    )
    expect(hostAuthLabel('host_auth_store')).toBe(
      'Host environment · CLI auth store'
    )
  })

  test('keeps unknown CLI auth methods explicit about their source', () => {
    expect(hostAuthLabel('future_method')).toBe(
      'Authenticated from the host environment'
    )
    expect(hostAuthLabel(null)).toBe('Authenticated from the host environment')
  })
})
