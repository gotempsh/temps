import { test, expect, describe } from 'bun:test'
import { parseEnvFile, formatEnvLine, findEnvironment } from './index.js'

describe('parseEnvFile', () => {
  test('parses simple KEY=value pairs', () => {
    expect(parseEnvFile('FOO=bar\nBAZ=qux')).toEqual({ FOO: 'bar', BAZ: 'qux' })
  })

  test('skips blank lines and comments', () => {
    expect(parseEnvFile('FOO=bar\n\n# a comment\nBAZ=qux')).toEqual({ FOO: 'bar', BAZ: 'qux' })
  })

  test('strips matching double quotes from the value', () => {
    expect(parseEnvFile('FOO="bar baz"')).toEqual({ FOO: 'bar baz' })
  })

  test('strips matching single quotes from the value', () => {
    expect(parseEnvFile("FOO='bar baz'")).toEqual({ FOO: 'bar baz' })
  })

  test('unescapes \\n and \\" inside a double-quoted value', () => {
    expect(parseEnvFile('FOO="line1\\nline2 says \\"hi\\""')).toEqual({
      FOO: 'line1\nline2 says "hi"',
    })
  })

  test('unescapes \\\' inside a single-quoted value', () => {
    expect(parseEnvFile("FOO='it\\'s here'")).toEqual({ FOO: "it's here" })
  })

  test('preserves the = sign inside the value', () => {
    expect(parseEnvFile('URL=postgres://user:pass@host/db?sslmode=require')).toEqual({
      URL: 'postgres://user:pass@host/db?sslmode=require',
    })
  })

  test('trims whitespace around keys and unquoted values', () => {
    expect(parseEnvFile('  FOO = bar  ')).toEqual({ FOO: 'bar' })
  })

  test('ignores lines with no = sign', () => {
    expect(parseEnvFile('FOO=bar\nnotanassignment\nBAZ=qux')).toEqual({ FOO: 'bar', BAZ: 'qux' })
  })

  test('returns an empty object for empty input', () => {
    expect(parseEnvFile('')).toEqual({})
  })

  test('allows an empty value', () => {
    expect(parseEnvFile('FOO=')).toEqual({ FOO: '' })
  })
})

describe('formatEnvLine', () => {
  test('leaves a plain value unquoted', () => {
    expect(formatEnvLine('FOO', 'bar')).toBe('FOO=bar')
  })

  test('renders an empty value as a bare assignment', () => {
    expect(formatEnvLine('FOO', '')).toBe('FOO=')
  })

  test('quotes a value containing a space', () => {
    expect(formatEnvLine('FOO', 'bar baz')).toBe('FOO="bar baz"')
  })

  test('quotes a value containing a #', () => {
    expect(formatEnvLine('FOO', 'bar#baz')).toBe('FOO="bar#baz"')
  })

  test('quotes and escapes a value containing a newline', () => {
    expect(formatEnvLine('FOO', 'line1\nline2')).toBe('FOO="line1\\nline2"')
  })

  test('quotes and escapes a value containing a double quote', () => {
    expect(formatEnvLine('FOO', 'say "hi"')).toBe('FOO="say \\"hi\\""')
  })

  test('round-trips through parseEnvFile for values with newlines and quotes', () => {
    const original = 'multi\nline "quoted" value'
    const line = formatEnvLine('FOO', original)
    expect(parseEnvFile(line)).toEqual({ FOO: original })
  })
})

describe('findEnvironment', () => {
  const envs = [
    { id: 1, name: 'Production', slug: 'production' },
    { id: 2, name: 'Preview', slug: 'preview' },
  ]

  test('matches by name case-insensitively', () => {
    expect(findEnvironment(envs, 'production')).toEqual(envs[0])
    expect(findEnvironment(envs, 'PRODUCTION')).toEqual(envs[0])
  })

  test('matches by exact slug', () => {
    expect(findEnvironment(envs, 'preview')).toEqual(envs[1])
  })

  test('returns undefined when nothing matches', () => {
    expect(findEnvironment(envs, 'staging')).toBeUndefined()
  })
})
