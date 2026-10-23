// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  availableArtifacts,
  buildCreateFunctionRequest,
  bytesToBase64,
  canSleep,
  canWake,
  functionNameError,
  functionStateBadge,
  functionUrl,
  wasmModuleError,
  type FunctionFormValues,
} from './functions'

function form(overrides: Partial<FunctionFormValues>): FunctionFormValues {
  return {
    name: 'resize-image',
    artifact: 'oci',
    image: 'registry.example.com/team/fn:1',
    port: '',
    idle_timeout_seconds: '',
    wake_timeout_seconds: '',
    memory_limit_mb: '',
    ...overrides,
  }
}

describe('functionNameError', () => {
  test('accepts the server rule', () => {
    expect(functionNameError('resize-image')).toBeNull()
    expect(functionNameError('a1')).toBeNull()
  })
  test('rejects what the server rejects', () => {
    for (const bad of [
      '',
      '1abc',
      'Resize',
      'a--b',
      'a-',
      'a_b',
      'a'.repeat(41),
    ]) {
      expect(functionNameError(bad)).not.toBeNull()
    }
  })
})

describe('buildCreateFunctionRequest', () => {
  test('an OCI function sends only what was set', () => {
    expect(buildCreateFunctionRequest(form({ port: '3000' }), null)).toEqual({
      ok: true,
      value: {
        name: 'resize-image',
        image: 'registry.example.com/team/fn:1',
        port: 3000,
      },
    })
  })

  test('a WASM function needs a module', () => {
    expect(
      buildCreateFunctionRequest(form({ artifact: 'wasm' }), null).ok
    ).toBe(false)
    const result = buildCreateFunctionRequest(
      form({ artifact: 'wasm' }),
      'AGFzbQ=='
    )
    expect(result.ok && result.value).toEqual({
      name: 'resize-image',
      wasm_base64: 'AGFzbQ==',
    })
  })

  test('range checks mirror the server', () => {
    expect(
      buildCreateFunctionRequest(form({ idle_timeout_seconds: '5' }), null).ok
    ).toBe(false)
    expect(
      buildCreateFunctionRequest(form({ wake_timeout_seconds: '301' }), null).ok
    ).toBe(false)
    expect(
      buildCreateFunctionRequest(form({ memory_limit_mb: '4097' }), null).ok
    ).toBe(false)
  })
})

describe('wasmModuleError', () => {
  test('accepts a core module header', () => {
    expect(
      wasmModuleError(new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0]))
    ).toBeNull()
  })
  test('rejects other files and components', () => {
    expect(wasmModuleError(new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]))).toContain(
      'not a WASM binary'
    )
    expect(
      wasmModuleError(new Uint8Array([0, 0x61, 0x73, 0x6d, 0x0d, 0, 1, 0]))
    ).toContain('wasm32-wasip1')
    expect(wasmModuleError(new Uint8Array())).toBe('The file is empty.')
  })
})

describe('helpers', () => {
  test('bytesToBase64', () => {
    expect(bytesToBase64(new Uint8Array([0, 0x61, 0x73, 0x6d]))).toBe(
      'AGFzbQ=='
    )
  })
  test('functionUrl', () => {
    expect(functionUrl('fn-a-b.example.test')).toBe(
      'https://fn-a-b.example.test'
    )
    expect(functionUrl('fn-a-b.localho.st', 'http:')).toBe(
      'http://fn-a-b.localho.st'
    )
  })
  test('state badges and actions', () => {
    expect(functionStateBadge('awake')).toEqual({
      label: 'Awake',
      tone: 'success',
    })
    expect(functionStateBadge('mystery').label).toBe('mystery')
    expect(canWake('asleep')).toBe(true)
    expect(canWake('awake')).toBe(false)
    expect(canSleep('awake')).toBe(true)
    expect(canSleep('unregistered')).toBe(false)
  })
  test('availableArtifacts', () => {
    expect(
      availableArtifacts({
        configured: true,
        wasm: { available: false, reason: 'not built with --features wasm' },
        oci: { available: true, reason: null },
        reason: null,
        setup_path: null,
      })
    ).toEqual(['oci'])
  })
})
