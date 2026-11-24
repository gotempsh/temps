// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  DEFAULT_SELF_HOSTED_CAPABILITIES,
  SERVER_DEFAULT_SELF_HOSTED_CAPABILITIES,
  hasIpLiteralHost,
  isDiscoverableBaseUrl,
  isPrivateAddressUrl,
  normalizeSelfHostedCapabilities,
  settingsSaveKeyId,
  isPlainHttpUrl,
  selfHostedDiscoverySignature,
  selfHostedSubmitBlocker,
} from './self-hosted-provider'

describe('self-hosted provider form helpers', () => {
  test('discovery is invalidated when base URL, key or trust change', () => {
    const base = {
      baseUrl: 'http://10.0.0.5:8000/v1',
      apiKey: '',
      trustPrivateAddress: false,
    }
    const signature = selfHostedDiscoverySignature(base)
    expect(
      selfHostedDiscoverySignature({
        ...base,
        baseUrl: ' http://10.0.0.5:8000/v1 ',
      })
    ).toBe(signature)
    expect(
      selfHostedDiscoverySignature({
        ...base,
        baseUrl: 'http://10.0.0.6:8000/v1',
      })
    ).not.toBe(signature)
    expect(selfHostedDiscoverySignature({ ...base, apiKey: 'k' })).not.toBe(
      signature
    )
    expect(
      selfHostedDiscoverySignature({ ...base, trustPrivateAddress: true })
    ).not.toBe(signature)
  })

  test('recognises plain http and IP-literal hosts', () => {
    expect(isPlainHttpUrl('HTTP://10.0.0.5/v1')).toBe(true)
    expect(isPlainHttpUrl('https://10.0.0.5/v1')).toBe(false)
    expect(hasIpLiteralHost('http://10.0.0.5:8000/v1')).toBe(true)
    expect(hasIpLiteralHost('http://[fd12::5]:8000/v1')).toBe(true)
    expect(hasIpLiteralHost('http://models.internal:8000/v1')).toBe(false)
    expect(hasIpLiteralHost('not a url')).toBe(false)
  })

  test('the form needs a discovered default model before it can be saved', () => {
    const models = ['mlx-community/Some-Model-4bit']
    expect(
      selfHostedSubmitBlocker({
        baseUrl: '',
        discoveredModels: null,
        defaultModel: '',
      })
    ).toContain('base URL')
    expect(
      selfHostedSubmitBlocker({
        baseUrl: 'http://10.0.0.5/v1',
        discoveredModels: null,
        defaultModel: '',
      })
    ).toContain('has not listed the models')
    expect(
      selfHostedSubmitBlocker({
        baseUrl: 'http://10.0.0.5/v1',
        discoveredModels: [],
        defaultModel: '',
      })
    ).toContain('no models')
    expect(
      selfHostedSubmitBlocker({
        baseUrl: 'http://10.0.0.5/v1',
        discoveredModels: models,
        defaultModel: 'gpt-5-nano',
      })
    ).toContain('Pick a default model')
    expect(
      selfHostedSubmitBlocker({
        baseUrl: 'http://10.0.0.5/v1',
        discoveredModels: models,
        defaultModel: models[0],
      })
    ).toBeNull()
  })

  test('stored capability declarations fall back to server defaults', () => {
    expect(normalizeSelfHostedCapabilities(null)).toEqual(
      SERVER_DEFAULT_SELF_HOSTED_CAPABILITIES
    )
    expect(
      normalizeSelfHostedCapabilities({ embeddings: true, streaming: null })
    ).toEqual({
      ...SERVER_DEFAULT_SELF_HOSTED_CAPABILITIES,
      embeddings: true,
    })
  })

  test('a new key declares every capability', () => {
    expect(Object.values(DEFAULT_SELF_HOSTED_CAPABILITIES)).toEqual([
      true,
      true,
      true,
      true,
    ])
  })

  test('only private endpoints are offered private address trust', () => {
    for (const url of [
      'http://10.0.0.5:8000/v1',
      'http://172.16.4.2/v1',
      'http://192.168.1.20:11434/v1',
      'http://127.0.0.1:8080/v1',
      'http://100.64.0.9/v1',
      'http://[::1]:8000/v1',
      'http://[fd12:3456::1]/v1',
      'http://[::ffff:10.0.0.5]/v1',
      'http://localhost:11434/v1',
      'http://ollama:11434/v1',
      'http://gpu-box.local/v1',
      'http://llm.internal/v1',
    ]) {
      expect(isPrivateAddressUrl(url)).toBe(true)
    }
    for (const url of [
      'https://api.example.com/v1',
      'https://8.8.8.8/v1',
      'http://172.32.0.1/v1',
      'http://[2001:db8::1]/v1',
      'not a url',
      '',
    ]) {
      expect(isPrivateAddressUrl(url)).toBe(false)
    }
  })

  test('pending settings saves are attributed to their key', () => {
    expect(settingsSaveKeyId({ id: 7, body: {} })).toBe(7)
    expect(settingsSaveKeyId(undefined)).toBeUndefined()
    expect(settingsSaveKeyId({ id: '7' })).toBeUndefined()
  })
})

describe('isDiscoverableBaseUrl', () => {
  test.each([
    ['https://llm.example.test/v1', true],
    ['http://10.0.0.5:8000/v1', true],
    ['  https://llm.example.test  ', true],
    ['', false],
    ['https://', false],
    ['llm.example.test/v1', false],
    ['ftp://llm.example.test', false],
    ['https://llm.', false],
  ])('%p -> %p', (url, expected) => {
    expect(isDiscoverableBaseUrl(url)).toBe(expected)
  })
})
