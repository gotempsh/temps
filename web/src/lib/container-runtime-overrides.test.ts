// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  containerRuntimeSchema,
  dockerfileCreationConfig,
  parseRuntimeArguments,
} from './container-runtime-overrides'

describe('container startup overrides', () => {
  test('preserves spaces, quotes, empty arguments and shell syntax verbatim', () => {
    const args = ['python', '-c', 'print("a b")', '', '$VALUE && echo ok']
    expect(parseRuntimeArguments(JSON.stringify(args))).toEqual(args)
    expect(
      containerRuntimeSchema.safeParse({
        command: JSON.stringify(args),
        entrypoint: '',
      }).success
    ).toBe(true)
  })
  test('blank inherits, while [] explicitly clears the entrypoint', () => {
    expect(parseRuntimeArguments('  ')).toBeNull()
    expect(parseRuntimeArguments('[]')).toEqual([])
    expect(
      containerRuntimeSchema.safeParse({ command: '', entrypoint: '[]' })
        .success
    ).toBe(true)
    expect(
      containerRuntimeSchema.safeParse({ command: '[]', entrypoint: '' })
        .success
    ).toBe(false)
  })
  test('rejects malformed, non-string, oversized and invalid executable arguments', () => {
    for (const command of [
      'python worker.py',
      '{}',
      'null',
      '[42]',
      '[""]',
      '["  "]',
      JSON.stringify(['python', '\0']),
      JSON.stringify(['a'.repeat(1025)]),
      JSON.stringify(Array(65).fill('a')),
    ]) {
      expect(
        containerRuntimeSchema.safeParse({ command, entrypoint: '' }).success
      ).toBe(false)
    }
  })
})

test('creation payload inherits blank fields and preserves explicit argument boundaries', () => {
  expect(dockerfileCreationConfig({})).toEqual({
    dockerfilePath: 'Dockerfile',
    command: null,
    entrypoint: null,
  })
  expect(
    dockerfileCreationConfig({
      dockerfilePath: 'docker/worker.Dockerfile',
      command: '["worker", "argument with spaces", ""]',
      entrypoint: '[]',
    })
  ).toEqual({
    dockerfilePath: 'docker/worker.Dockerfile',
    command: ['worker', 'argument with spaces', ''],
    entrypoint: [],
  })
  expect(dockerfileCreationConfig({ entrypoint: '["/app/start"]' })).toEqual({
    dockerfilePath: 'Dockerfile',
    command: null,
    entrypoint: ['/app/start'],
  })
})
