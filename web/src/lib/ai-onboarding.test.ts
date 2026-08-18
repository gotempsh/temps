import { describe, expect, test } from 'bun:test'
import {
  AI_HARNESS_KEY_NAME,
  buildAiHarnessCommands,
  buildAiHarnessKeyRequest,
  findAiHarnessKey,
  getAiHarnessStatus,
} from './ai-onboarding'

describe('AI harness onboarding', () => {
  test('installs only the canonical Temps skill', () => {
    expect(
      buildAiHarnessCommands('https://temps.example.com').installSkill
    ).toBe('bunx skills add gotempsh/temps --skill temps')
  })

  test('connects the pinned CLI without putting the API key in command text', () => {
    const commands = buildAiHarnessCommands('https://temps.example.com/')

    expect(commands.connectCli).toContain(
      'bunx @temps-sdk/cli@0.1.34 login https://temps.example.com'
    )
    expect(commands.connectCli).toContain('--api-key "$TEMPS_API_KEY"')
    expect(commands.connectCli).toContain('--context temps-example-com')
    expect(commands.connectCli).toContain('unset TEMPS_API_KEY')
  })

  test('verifies the same explicit target context', () => {
    const commands = buildAiHarnessCommands('http://localhost:3003')

    expect(commands.verifyIdentity).toBe(
      'bunx @temps-sdk/cli@0.1.34 --target-context localhost whoami'
    )
    expect(commands.verifyProjects).toContain(
      '--target-context localhost projects list --json'
    )
  })

  test('creates the dedicated harness key with admin access', () => {
    expect(buildAiHarnessKeyRequest()).toEqual({
      name: AI_HARNESS_KEY_NAME,
      role_type: 'admin',
    })
  })

  test('requires the dedicated key to make an authenticated request', () => {
    const key = {
      id: 12,
      name: AI_HARNESS_KEY_NAME,
      role_type: 'ADMIN',
      is_active: true,
      last_used_at: null,
    }

    expect(findAiHarnessKey([key])?.id).toBe(12)
    expect(getAiHarnessStatus([key])).toBe('waiting')
    expect(
      getAiHarnessStatus([{ ...key, last_used_at: '2026-08-16T12:00:00Z' }])
    ).toBe('connected')
  })

  test('ignores inactive, non-admin, and unrelated keys', () => {
    expect(
      getAiHarnessStatus([
        {
          id: 1,
          name: AI_HARNESS_KEY_NAME,
          role_type: 'user',
          is_active: true,
          last_used_at: '2026-08-16T12:00:00Z',
        },
        {
          id: 2,
          name: AI_HARNESS_KEY_NAME,
          role_type: 'admin',
          is_active: false,
          last_used_at: '2026-08-16T12:00:00Z',
        },
      ])
    ).toBe('missing')
  })
})
