// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import {
  buildCreateBody,
  describeCapability,
  parseIntFlag,
  registerFunctionsCommands,
  validateCreateOptions,
} from './index.js'

describe('validateCreateOptions', () => {
  test('requires a name', () => {
    expect(() => validateCreateOptions({ image: 'reg/fn:1' })).toThrow('--name is required')
    expect(() => validateCreateOptions({ name: '  ', image: 'reg/fn:1' })).toThrow('--name is required')
  })

  test('refuses neither artifact', () => {
    expect(() => validateCreateOptions({ name: 'fn' })).toThrow('exactly one of --wasm')
  })

  test('refuses both artifacts', () => {
    expect(() => validateCreateOptions({ name: 'fn', wasm: 'a.wasm', image: 'reg/fn:1' })).toThrow(
      'exactly one of --wasm',
    )
  })

  test('accepts exactly one artifact', () => {
    expect(() => validateCreateOptions({ name: 'fn', image: 'reg/fn:1' })).not.toThrow()
    expect(() => validateCreateOptions({ name: 'fn', wasm: 'a.wasm' })).not.toThrow()
  })
})

describe('buildCreateBody', () => {
  test('an image function carries the image and no wasm', () => {
    const body = buildCreateBody({ name: ' fn ', image: ' reg/fn:1 ', port: '9000' })
    expect(body).toEqual({ name: 'fn', image: 'reg/fn:1', port: 9000 })
  })

  test('a wasm function sends the module base64-encoded', () => {
    const bytes = new Uint8Array([0x00, 0x61, 0x73, 0x6d])
    const body = buildCreateBody({ name: 'fn', wasm: 'm.wasm' }, bytes)
    expect(body.wasm_base64).toBe(Buffer.from(bytes).toString('base64'))
    expect(body.image).toBeUndefined()
  })

  test('an empty wasm file is refused before any request', () => {
    expect(() => buildCreateBody({ name: 'fn', wasm: 'm.wasm' }, new Uint8Array())).toThrow(
      '--wasm file m.wasm is empty',
    )
  })

  test('limits map onto the request fields', () => {
    const body = buildCreateBody({
      name: 'fn',
      image: 'reg/fn:1',
      memory: '256',
      idleTimeout: '600',
      wakeTimeout: '45',
    })
    expect(body.memory_limit_mb).toBe(256)
    expect(body.idle_timeout_seconds).toBe(600)
    expect(body.wake_timeout_seconds).toBe(45)
  })

  test('out-of-range limits name the flag', () => {
    expect(() => buildCreateBody({ name: 'fn', image: 'r', idleTimeout: '5' })).toThrow(
      '--idle-timeout must be between 10 and 86400',
    )
    expect(() => buildCreateBody({ name: 'fn', image: 'r', memory: '8192' })).toThrow(
      '--memory must be between 1 and 4096',
    )
  })
})

describe('parseIntFlag', () => {
  test('absent flag is undefined', () => {
    expect(parseIntFlag('--port', undefined, 1, 65535)).toBeUndefined()
  })

  test('non-integers are refused instead of becoming NaN', () => {
    expect(() => parseIntFlag('--port', '80a', 1, 65535)).toThrow('--port must be an integer')
    expect(() => parseIntFlag('--port', '1.5', 1, 65535)).toThrow('--port must be an integer')
  })
})

describe('describeCapability', () => {
  test('names each runtime and why it is unavailable', () => {
    const lines = describeCapability({
      configured: true,
      wasm: { available: true },
      oci: { available: false, reason: 'No worker runs containers' },
    })
    expect(lines).toEqual(['WASM: available', 'OCI image: unavailable (No worker runs containers)'])
  })
})

describe('registerFunctionsCommands', () => {
  test('registers every subcommand with its flags', () => {
    const program = new Command()
    registerFunctionsCommands(program)
    const functions = program.commands.find((c) => c.name() === 'functions')!
    expect(functions).toBeDefined()
    const names = functions.commands.map((c) => c.name()).sort()
    expect(names).toEqual(['capability', 'create', 'list', 'rm', 'show', 'sleep', 'wake'])
    const create = functions.commands.find((c) => c.name() === 'create')!
    const flags = create.options.map((o) => o.long)
    for (const flag of ['--project', '--name', '--wasm', '--image', '--port', '--memory', '--idle-timeout', '--wake-timeout']) {
      expect(flags).toContain(flag)
    }
  })
})
