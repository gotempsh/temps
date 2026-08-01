import { test, expect, describe, beforeEach, afterEach } from 'bun:test'
import {
  pickActiveContext,
  envContextName,
  contextExists,
  __resetMissingContextWarning,
  type CliContext,
} from './contexts.js'

function ctx(name: string, extra: Partial<CliContext> = {}): CliContext {
  return {
    name,
    url: `https://${name}.example.com`,
    apiKey: `tk_${name}`,
    email: `${name}@example.com`,
    ...extra,
  }
}

const ENV_KEY = 'TEMPS_CONTEXT'
const originalEnv = process.env[ENV_KEY]
// Swallow the one-time stderr warning so test output stays clean. We still
// assert the *behavior* (null result) on the miss path.
const originalStderrWrite = process.stderr.write.bind(process.stderr)

beforeEach(() => {
  delete process.env[ENV_KEY]
  __resetMissingContextWarning()
  process.stderr.write = (() => true) as typeof process.stderr.write
})

afterEach(() => {
  if (originalEnv === undefined) delete process.env[ENV_KEY]
  else process.env[ENV_KEY] = originalEnv
  process.stderr.write = originalStderrWrite
})

describe('envContextName', () => {
  test('returns null when unset', () => {
    expect(envContextName()).toBeNull()
  })

  test('returns trimmed value when set', () => {
    process.env[ENV_KEY] = '  prod  '
    expect(envContextName()).toBe('prod')
  })

  test('treats empty / whitespace-only as unset', () => {
    process.env[ENV_KEY] = '   '
    expect(envContextName()).toBeNull()
  })
})

describe('pickActiveContext', () => {
  const contexts = [
    ctx('local', { isActive: true }),
    ctx('prod'),
    ctx('stage'),
  ]

  test('falls back to the isActive flag with no env var', () => {
    expect(pickActiveContext(contexts)?.name).toBe('local')
  })

  test('falls back to the first context when none is flagged active', () => {
    const unflagged = [ctx('a'), ctx('b')]
    expect(pickActiveContext(unflagged)?.name).toBe('a')
  })

  test('returns null for an empty list', () => {
    expect(pickActiveContext([])).toBeNull()
  })

  test('TEMPS_CONTEXT selects a matching context, overriding the active flag', () => {
    process.env[ENV_KEY] = 'prod'
    const picked = pickActiveContext(contexts)
    expect(picked?.name).toBe('prod')
    // It overrides `local` even though `local` is the on-disk active one.
    expect(picked?.url).toBe('https://prod.example.com')
    expect(picked?.apiKey).toBe('tk_prod')
  })

  test('TEMPS_CONTEXT naming a missing context returns null (no silent fallback)', () => {
    process.env[ENV_KEY] = 'does-not-exist'
    // Critical: we do NOT fall back to `local`/first — that would silently
    // point the CLI at the wrong server.
    expect(pickActiveContext(contexts)).toBeNull()
  })

  test('whitespace-only TEMPS_CONTEXT is ignored, active flag wins', () => {
    process.env[ENV_KEY] = '   '
    expect(pickActiveContext(contexts)?.name).toBe('local')
  })
})

describe('contextExists', () => {
  const contexts = [ctx('local'), ctx('prod'), ctx('stage')]

  test('true for a configured name', () => {
    expect(contextExists('prod', contexts)).toBe(true)
  })

  test('false for a typo / unconfigured name', () => {
    expect(contextExists('production', contexts)).toBe(false)
  })

  test('false against an empty list', () => {
    expect(contextExists('local', [])).toBe(false)
  })
})
