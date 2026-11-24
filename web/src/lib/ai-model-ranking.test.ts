// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

  test('puts the current generation of every vendor first', () => {
    expect(
      sortAiModelIdsByRelevance([
        'gpt-5.6-sol',
        'gpt-6-luna',
        'gpt-6.2-sol',
        'gpt-image-2',
      ])
    ).toEqual(['gpt-6.2-sol', 'gpt-6-luna', 'gpt-5.6-sol', 'gpt-image-2'])
    expect(
      sortAiModelIdsByRelevance([
        'claude-haiku-4-5',
        'claude-mythos-5-2',
        'claude-opus-5',
        'claude-opus-5-5',
      ])
    ).toEqual([
      'claude-opus-5-5',
      'claude-mythos-5-2',
      'claude-opus-5',
      'claude-haiku-4-5',
    ])
    expect(
      sortAiModelIdsByRelevance(['gemini-3.8-flash', 'gemini-4-neon'])
    ).toEqual(['gemini-4-neon', 'gemini-3.8-flash'])
  })

  test('never reads a dated snapshot as a newer minor version', () => {
    expect(
      sortAiModelIdsByRelevance([
        'claude-mythos-5-20261101',
        'claude-sonnet-4-20250514',
        'claude-mythos-5-2',
        'claude-opus-5-5',
      ])
    ).toEqual([
      'claude-opus-5-5',
      'claude-mythos-5-2',
      'claude-mythos-5-20261101',
      'claude-sonnet-4-20250514',
    ])
    expect(
      sortAiModelIdsByRelevance([
        'gpt-5-2025-08-07',
        'gpt-6.2-sol',
        'gpt-5.6-sol',
      ])
    ).toEqual(['gpt-6.2-sol', 'gpt-5.6-sol', 'gpt-5-2025-08-07'])
  })
})
