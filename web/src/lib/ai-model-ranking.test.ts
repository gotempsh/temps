import { describe, expect, test } from 'bun:test'
import { sortAiModelIdsByRelevance } from './ai-model-ranking'

describe('AI model relevance sorting', () => {
  test('puts current OpenAI text models ahead of legacy and image models', () => {
    expect(
      sortAiModelIdsByRelevance([
        'chatgpt-image-latest',
        'gpt-3.5-turbo',
        'gpt-5.4-mini',
        'gpt-5.6-luna',
        'gpt-5.6-sol',
        'gpt-5.2-2025-12-11',
      ])
    ).toEqual([
      'gpt-5.6-sol',
      'gpt-5.6-luna',
      'gpt-5.4-mini',
      'gpt-5.2-2025-12-11',
      'gpt-3.5-turbo',
      'chatgpt-image-latest',
    ])
  })

  test('sorts other provider families newest-first', () => {
    expect(
      sortAiModelIdsByRelevance([
        'gemini-2.5-flash',
        'gemini-3.6-flash',
        'gemini-3.5-flash',
      ])
    ).toEqual(['gemini-3.6-flash', 'gemini-3.5-flash', 'gemini-2.5-flash'])
  })
})
