// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { AssistantModelOption } from './assistant-models'
import {
  featuredModels,
  filterByVendor,
  modelVendor,
  orderByPopularity,
  shortModelName,
  vendorOptions,
} from './model-vendors'

const option = (id: string): AssistantModelOption => ({
  id,
  name: id,
  details: {},
})

describe('modelVendor', () => {
  it('reads the vendor from the id prefix and gives known ones their mark', () => {
    expect(modelVendor('google/gemini-3.5-flash')).toEqual({
      key: 'google',
      name: 'Google',
      logo: 'google.svg',
    })
    expect(modelVendor('mistralai/mistral-large').name).toBe('Mistral')
  })

  it('treats a router alias as its vendor and merges vendors that share a name', () => {
    expect(modelVendor('~openai/gpt-latest').key).toBe('openai')
    expect(modelVendor('meta/llama-x').key).toBe(
      modelVendor('meta-llama/llama-y').key
    )
  })

  it('names an unknown vendor from its slug and gives it no mark', () => {
    expect(modelVendor('sao10k/some-model')).toEqual({
      key: 'sao10k',
      name: 'Sao10k',
      logo: null,
    })
    expect(modelVendor('thinking-machines/x').name).toBe('Thinking Machines')
  })

  it('recognises well-known families without a vendor prefix', () => {
    expect(modelVendor('claude-sonnet-5-5').key).toBe('anthropic')
    expect(modelVendor('gpt-4o').key).toBe('openai')
    expect(modelVendor('gemini-3-pro').key).toBe('google')
    expect(modelVendor('something-else').key).toBe('other')
  })
})

describe('vendorOptions', () => {
  it('lists every vendor, counting aliases with their vendor', () => {
    const vendors = vendorOptions([
      option('google/a'),
      option('openai/a'),
      option('openai/b'),
      option('~openai/c'),
    ])
    expect(vendors.map((vendor) => [vendor.name, vendor.count])).toEqual([
      ['OpenAI', 3],
      ['Google', 1],
    ])
  })

  it('does not fold a long tail of vendors away', () => {
    const options = Array.from({ length: 30 }, (_, index) =>
      option(`vendor${String(index).padStart(2, '0')}/model`)
    )
    expect(vendorOptions(options)).toHaveLength(30)
  })
})

describe('filterByVendor', () => {
  const options = [option('google/a'), option('openai/a'), option('~openai/b')]

  it('keeps every model without a vendor and one vendor with one', () => {
    expect(filterByVendor(options, null)).toHaveLength(3)
    expect(filterByVendor(options, 'openai').map((o) => o.id)).toEqual([
      'openai/a',
      '~openai/b',
    ])
  })
})

describe('shortModelName', () => {
  it('drops the vendor prefix the logo already shows', () => {
    expect(
      shortModelName('Anthropic: Claude Opus 5.5', 'anthropic/claude-opus-5.5')
    ).toBe('Claude Opus 5.5')
    expect(
      shortModelName(
        'DeepSeek: DeepSeek V4.1 Flash',
        'deepseek/deepseek-v4.1-flash'
      )
    ).toBe('DeepSeek V4.1 Flash')
    expect(shortModelName('AionLabs: Aion 3.5', 'aion-labs/aion-3.5')).toBe(
      'Aion 3.5'
    )
  })

  it('matches the prefix against the id slug too', () => {
    expect(shortModelName('Mistral AI: Large', 'mistralai/mistral-large')).toBe(
      'Large'
    )
  })

  it('leaves a name alone when the prefix is something else or nothing would remain', () => {
    expect(
      shortModelName('Preview: Claude Opus', 'anthropic/claude-opus')
    ).toBe('Preview: Claude Opus')
    expect(shortModelName('Claude Opus', 'anthropic/claude-opus')).toBe(
      'Claude Opus'
    )
    expect(shortModelName('Anthropic: ', 'anthropic/x')).toBe('Anthropic: ')
  })
})

describe('vendor order', () => {
  it('puts the best-known vendors ahead of bigger unknown ones in the filter', () => {
    const options = [
      ...Array.from({ length: 5 }, (_, i) => option(`aion-labs/model-${i}`)),
      option('anthropic/claude-sonnet-5'),
      option('openai/gpt-5'),
    ]
    expect(vendorOptions(options).map((vendor) => vendor.name)).toEqual([
      'Anthropic',
      'OpenAI',
      'Aion Labs',
    ])
  })
})

describe('orderByPopularity', () => {
  it('lists known vendors first and the newest version first within one', () => {
    const ids = orderByPopularity([
      option('ai21/jamba'),
      option('openai/gpt-4o'),
      option('anthropic/claude-sonnet-4'),
      option('openai/gpt-5'),
      option('anthropic/claude-sonnet-10'),
      option('google/gemini-3-flash'),
    ]).map((o) => o.id)
    expect(ids).toEqual([
      'anthropic/claude-sonnet-10',
      'anthropic/claude-sonnet-4',
      'openai/gpt-5',
      'openai/gpt-4o',
      'google/gemini-3-flash',
      'ai21/jamba',
    ])
  })
})

describe('featuredModels', () => {
  it('picks the newest of each family without preview, alias or batch variants', () => {
    const featured = featuredModels([
      option('anthropic/claude-sonnet-4'),
      option('anthropic/claude-sonnet-5'),
      option('anthropic/claude-sonnet-6-preview'),
      option('anthropic/claude-haiku-5'),
      option('openai/gpt-5'),
      option('openai/gpt-5.5'),
      option('openai/gpt-5.5-mini'),
      option('~openai/gpt-latest'),
      option('ai21/jamba'),
    ]).map((o) => o.id)
    expect(featured).toEqual([
      'anthropic/claude-sonnet-5',
      'anthropic/claude-haiku-5',
      'openai/gpt-5.5',
      'openai/gpt-5.5-mini',
    ])
  })

  it('skips a model whose catalogue says it cannot call tools', () => {
    const noTools: AssistantModelOption = {
      id: 'anthropic/claude-opus-9',
      name: 'x',
      details: { supports_tools: false },
    }
    expect(featuredModels([noTools])).toEqual([])
  })

  it('is empty when the catalogue lists none of the featured families', () => {
    expect(featuredModels([option('ai21/jamba')])).toEqual([])
  })
})
