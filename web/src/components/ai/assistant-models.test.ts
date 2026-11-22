// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  assistantModelOptions,
  describeModel,
  filterAssistantModels,
  formatContextLength,
  formatModalities,
  formatPricePerMillion,
  imageSupport,
  lacksToolCalling,
  modelDisplayName,
  type AssistantModelRow,
} from './assistant-models'

const row = (overrides: Partial<AssistantModelRow>): AssistantModelRow => ({
  model_id: 'vendor/model-a',
  display_name: 'vendor/model-a',
  is_available: true,
  is_enabled: true,
  ...overrides,
})

describe('assistantModelOptions', () => {
  it('lists only models that are available and enabled, never embeddings', () => {
    const options = assistantModelOptions([
      row({ model_id: 'a/one', display_name: 'a/one' }),
      row({ model_id: 'a/off', display_name: 'a/off', is_enabled: false }),
      row({ model_id: 'a/gone', display_name: 'a/gone', is_available: false }),
      row({ model_id: 'text-embedding-x', display_name: 'text-embedding-x' }),
    ])
    expect(options.map((option) => option.id)).toEqual(['a/one'])
  })

  it('leaves out batch-only variants, which cannot answer a chat', () => {
    const options = assistantModelOptions([
      row({ model_id: 'a/one', display_name: 'a/one' }),
      row({ model_id: 'a/one:batch', display_name: 'a/one:batch' }),
    ])
    expect(options.map((option) => option.id)).toEqual(['a/one'])
  })

  it('prefers the catalogue name and sorts by it', () => {
    const options = assistantModelOptions([
      row({ model_id: 'z/last', display_name: 'z/last' }),
      row({
        model_id: 'a/second',
        display_name: 'a/second',
        details: { name: 'Alpha Model' },
      }),
    ])
    expect(options.map((option) => option.name)).toEqual([
      'Alpha Model',
      'z/last',
    ])
  })

  it('handles a missing list', () => {
    expect(assistantModelOptions(undefined)).toEqual([])
  })
})

describe('filterAssistantModels', () => {
  const options = assistantModelOptions([
    row({
      model_id: 'a/fast',
      display_name: 'a/fast',
      details: { name: 'Fast' },
    }),
    row({ model_id: 'b/deep', display_name: 'b/deep' }),
  ])

  it('matches name or id, ignoring case', () => {
    expect(filterAssistantModels(options, 'FAST').map((o) => o.id)).toEqual([
      'a/fast',
    ])
    expect(filterAssistantModels(options, 'b/').map((o) => o.id)).toEqual([
      'b/deep',
    ])
  })

  it('returns everything for a blank query and nothing for a miss', () => {
    expect(filterAssistantModels(options, '  ')).toHaveLength(2)
    expect(filterAssistantModels(options, 'zzz')).toHaveLength(0)
  })
})

describe('formatting', () => {
  it('names modalities', () => {
    expect(formatModalities(['text', 'image'])).toBe('Text, Image')
    expect(formatModalities(['custom'])).toBe('custom')
    expect(formatModalities(undefined)).toBe('')
  })

  it('shortens context windows', () => {
    expect(formatContextLength(200_000)).toBe('200K')
    expect(formatContextLength(1_048_576)).toBe('1M')
    expect(formatContextLength(8_192)).toBe('8.2K')
    expect(formatContextLength(512)).toBe('512')
    expect(formatContextLength(null)).toBeNull()
    expect(formatContextLength(0)).toBeNull()
  })

  it('formats prices per million tokens', () => {
    expect(formatPricePerMillion(3)).toBe('$3')
    expect(formatPricePerMillion(15)).toBe('$15')
    expect(formatPricePerMillion(0.25)).toBe('$0.25')
    expect(formatPricePerMillion(2.5)).toBe('$2.5')
    expect(formatPricePerMillion(0)).toBe('Free')
    expect(formatPricePerMillion(-1)).toBeNull()
    expect(formatPricePerMillion(undefined)).toBeNull()
  })
})

describe('describeModel', () => {
  it('reports input and output together with cost', () => {
    const [option] = assistantModelOptions([
      row({
        details: {
          input_modalities: ['text', 'image'],
          output_modalities: ['text'],
          context_length: 200_000,
          input_price_per_million: 3,
          output_price_per_million: 15,
        },
      }),
    ])
    expect(describeModel(option)).toEqual([
      'In: Text, Image',
      'Out: Text',
      '200K context',
      '$3 in · $15 out per 1M tokens',
    ])
  })

  it('leaves out what the catalogue does not publish', () => {
    const [option] = assistantModelOptions([row({})])
    expect(describeModel(option)).toEqual([])
  })
})

describe('tool calling', () => {
  it('warns only when the catalogue says tools are unsupported', () => {
    const [no, yes, unknown] = assistantModelOptions([
      row({
        model_id: 'a/no',
        display_name: 'a/no',
        details: { supports_tools: false },
      }),
      row({
        model_id: 'b/yes',
        display_name: 'b/yes',
        details: { supports_tools: true },
      }),
      row({ model_id: 'c/unknown', display_name: 'c/unknown' }),
    ])
    expect(lacksToolCalling(no)).toBe(true)
    expect(lacksToolCalling(yes)).toBe(false)
    expect(lacksToolCalling(unknown)).toBe(false)
  })
})

describe('modelDisplayName', () => {
  it('resolves a known id and falls back to the id', () => {
    const options = assistantModelOptions([
      row({
        model_id: 'a/one',
        display_name: 'a/one',
        details: { name: 'One' },
      }),
    ])
    expect(modelDisplayName(options, 'a/one')).toBe('One')
    expect(modelDisplayName(options, 'x/other')).toBe('x/other')
    expect(modelDisplayName(options, null)).toBeNull()
  })
})

describe('imageSupport', () => {
  it('follows the catalogue input modalities', () => {
    expect(imageSupport({ input_modalities: ['text', 'image'] })).toBe('yes')
    expect(imageSupport({ input_modalities: ['TEXT', 'Image'] })).toBe('yes')
    expect(imageSupport({ input_modalities: ['text'] })).toBe('no')
  })

  it('gives a model the catalogue is silent about the benefit of the doubt', () => {
    expect(imageSupport({ input_modalities: [] })).toBe('unknown')
    expect(imageSupport({})).toBe('unknown')
    expect(imageSupport(null)).toBe('unknown')
    expect(imageSupport(undefined)).toBe('unknown')
  })
})
