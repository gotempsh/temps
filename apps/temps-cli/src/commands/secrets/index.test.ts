import { test, expect, describe, afterEach } from 'bun:test'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { buildCreateSecretBody, buildUpdateSecretBody, resolveValue } from './index.js'

const temporaryDirectories: string[] = []

afterEach(async () => {
  await Promise.all(
    temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true })),
  )
})

async function fixtureFile(contents: string): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), 'temps-secrets-test-'))
  temporaryDirectories.push(directory)
  const path = join(directory, 'creds.json')
  await writeFile(path, contents)
  return path
}

describe('resolveValue', () => {
  test('passes a plain value through untouched', () => {
    expect(resolveValue('sk-live-123')).toBe('sk-live-123')
  })

  test('reads file contents when prefixed with @', async () => {
    const path = await fixtureFile('{"key":"value"}')
    expect(resolveValue(`@${path}`)).toBe('{"key":"value"}')
  })

  test('raises a clear error naming the file that could not be read', () => {
    expect(() => resolveValue('@/nonexistent/path/creds.json')).toThrow(
      /Failed to read file '\/nonexistent\/path\/creds.json'/,
    )
  })
})

describe('buildCreateSecretBody', () => {
  test('defaults to env type and resolves a plain value', () => {
    const body = buildCreateSecretBody({ name: 'my_key', value: 'secret-value' })
    expect(body).toEqual({ name: 'my_key', secret_type: 'env', value: 'secret-value' })
  })

  test('rejects an invalid --type instead of silently coercing it', () => {
    expect(() =>
      buildCreateSecretBody({ name: 'my_key', value: 'v', type: 'bogus' }),
    ).toThrow(/Invalid --type: must be "env" or "file"/)
  })

  test('requires --mount-path for file-type secrets', () => {
    expect(() =>
      buildCreateSecretBody({ name: 'creds', value: 'v', type: 'file' }),
    ).toThrow(/--mount-path is required when --type is "file"/)
  })

  test('carries mount_path and description through for file-type secrets', () => {
    const body = buildCreateSecretBody({
      name: 'creds',
      value: 'v',
      type: 'FILE',
      mountPath: '/run/secrets/creds.json',
      description: 'GSC creds',
    })
    expect(body).toEqual({
      name: 'creds',
      secret_type: 'file',
      value: 'v',
      mount_path: '/run/secrets/creds.json',
      description: 'GSC creds',
    })
  })

  test('resolves an @-prefixed value from a file', async () => {
    const path = await fixtureFile('file-contents')
    const body = buildCreateSecretBody({ name: 'my_key', value: `@${path}` })
    expect(body.value).toBe('file-contents')
  })
})

describe('buildUpdateSecretBody', () => {
  const current = {
    id: 1,
    name: 'my_key',
    secret_type: 'env' as const,
    value: '***',
    mount_path: null,
    description: 'old description',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  }

  test('requires --value since the server never echoes the existing one', () => {
    expect(() => buildUpdateSecretBody({ name: 'my_key' }, current)).toThrow(
      /--value is required when updating/,
    )
  })

  test('falls back to the current type and description when not overridden', () => {
    const body = buildUpdateSecretBody({ name: 'my_key', value: 'new-value' }, current)
    expect(body).toEqual({
      name: 'my_key',
      secret_type: 'env',
      value: 'new-value',
      description: 'old description',
    })
  })

  test('rejects an invalid --type override', () => {
    expect(() =>
      buildUpdateSecretBody({ name: 'my_key', value: 'v', type: 'bogus' }, current),
    ).toThrow(/Invalid --type: must be "env" or "file"/)
  })

  test('requires a mount path when switching an existing secret to file type', () => {
    expect(() =>
      buildUpdateSecretBody({ name: 'my_key', value: 'v', type: 'file' }, current),
    ).toThrow(/--mount-path is required when type is "file"/)
  })

  test('keeps the existing mount path for a file-type secret when not overridden', () => {
    const fileSecret = { ...current, secret_type: 'file' as const, mount_path: '/run/secrets/x.json' }
    const body = buildUpdateSecretBody({ name: 'my_key', value: 'v' }, fileSecret)
    expect(body.mount_path).toBe('/run/secrets/x.json')
  })

  test('overrides the mount path when a new one is provided', () => {
    const fileSecret = { ...current, secret_type: 'file' as const, mount_path: '/run/secrets/old.json' }
    const body = buildUpdateSecretBody(
      { name: 'my_key', value: 'v', mountPath: '/run/secrets/new.json' },
      fileSecret,
    )
    expect(body.mount_path).toBe('/run/secrets/new.json')
  })
})
