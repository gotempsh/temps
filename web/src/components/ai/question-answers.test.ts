// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  chooseOption,
  chooseOther,
  composeAnswer,
  EMPTY_DRAFT,
  isAnswered,
} from './question-answers'

describe('single-select questions', () => {
  test('choosing an option replaces the previous one', () => {
    const draft = chooseOption(
      chooseOption(EMPTY_DRAFT, 'Google', false),
      'Okta',
      false
    )
    expect(draft.selected).toEqual(['Okta'])
    expect(composeAnswer(draft)).toBe('Okta')
  })

  test('Other replaces the chosen option and needs text to count', () => {
    let draft = chooseOther(chooseOption(EMPTY_DRAFT, 'Google', false), false)
    expect(draft.selected).toEqual([])
    expect(isAnswered(draft)).toBe(false)

    draft = { ...draft, otherText: '  A self-hosted Authentik  ' }
    expect(isAnswered(draft)).toBe(true)
    expect(composeAnswer(draft)).toBe('A self-hosted Authentik')
  })

  test('choosing an option after Other drops the written answer', () => {
    const draft = chooseOption(
      { ...chooseOther(EMPTY_DRAFT, false), otherText: 'typed' },
      'Okta',
      false
    )
    expect(draft.otherChosen).toBe(false)
    expect(composeAnswer(draft)).toBe('Okta')
  })

  test('nothing chosen is not an answer', () => {
    expect(isAnswered(EMPTY_DRAFT)).toBe(false)
    expect(composeAnswer(EMPTY_DRAFT)).toBe('')
  })
})

describe('multi-select questions', () => {
  test('options toggle and combine with the written answer', () => {
    let draft = chooseOption(EMPTY_DRAFT, 'Email', true)
    draft = chooseOption(draft, 'Slack', true)
    draft = chooseOption(draft, 'Email', true)
    draft = { ...chooseOther(draft, true), otherText: 'Webhook' }
    expect(composeAnswer(draft)).toBe('Slack, Webhook')
    expect(isAnswered(draft)).toBe(true)
  })

  test('an open but empty Other blocks sending until it is filled or unticked', () => {
    const open = chooseOther(chooseOption(EMPTY_DRAFT, 'Email', true), true)
    expect(isAnswered(open)).toBe(false)
    expect(isAnswered(chooseOther(open, true))).toBe(true)
  })
})
