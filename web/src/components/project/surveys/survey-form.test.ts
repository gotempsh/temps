// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { SurveyResponse } from '@/api/client'
import {
  blankQuestion,
  describeTargeting,
  draftFromSurvey,
  draftToBody,
  emptyDraft,
  formatAnswer,
  hasErrors,
  nextQuestionId,
  percentOf,
  statusActions,
  validateDraft,
} from './survey-form'

describe('survey form', () => {
  test('a new draft is a valid one-question NPS survey once named', () => {
    const draft = { ...emptyDraft(), name: 'How are we doing?' }
    expect(hasErrors(validateDraft(draft, true))).toBe(false)
    expect(draftToBody(draft)).toEqual({
      name: 'How are we doing?',
      description: null,
      questions: [
        {
          id: 'q1',
          kind: 'nps',
          prompt: 'How likely are you to recommend us to a friend?',
          optional: false,
        },
      ],
      targeting: {
        path_pattern: null,
        trigger_event: null,
        sample_rate: 1,
        once_per_visitor: true,
        flag: null,
      },
    })
  })

  test('question ids never collide and never reuse a lower number', () => {
    const first = blankQuestion([])
    expect(first.id).toBe('q1')
    const second = { ...blankQuestion([first]), id: 'q3' }
    // q2 may have been removed; its answers are stored under "q2".
    expect(nextQuestionId([first, second])).toBe('q4')
    expect(nextQuestionId([{ ...first, id: 'rating' }])).toBe('q2')
  })

  test('reports each problem next to its field', () => {
    const draft = {
      ...emptyDraft(),
      name: ' ',
      pathPattern: 'checkout',
      triggerEvent: 'has space',
      samplePercent: '150',
      flagKey: 'beta',
      flagValue: 'yes',
      questions: [
        {
          ...blankQuestion([], 'single_choice'),
          prompt: 'Plan?',
          choices: ['A', 'A'],
        },
        { ...blankQuestion([], 'open_text'), id: 'q1', prompt: 'Why?' },
      ],
    }
    const errors = validateDraft(draft, true)
    expect(errors.name).toBeDefined()
    expect(errors.pathPattern).toContain("'/'")
    expect(errors.triggerEvent).toBeDefined()
    expect(errors.samplePercent).toBeDefined()
    expect(errors.flagValue).toBeDefined()
    expect(errors.question[0]).toBe('Each choice must be different.')
    expect(errors.question[1]).toContain('already uses the id')
  })

  test('an active survey needs a question but a draft does not', () => {
    const draft = { ...emptyDraft(), name: 'Later', questions: [] }
    expect(validateDraft(draft, true).questions).toBeDefined()
    expect(hasErrors(validateDraft(draft, false))).toBe(false)
  })

  test('round-trips a stored survey', () => {
    const survey: SurveyResponse = {
      id: 3,
      project_id: 1,
      name: 'Checkout',
      description: null,
      status: 'active',
      questions: [
        {
          id: 'stars',
          kind: 'rating',
          prompt: 'Rate it',
          optional: true,
          scale: 10,
          lower_label: 'Bad',
        },
        {
          id: 'plan',
          kind: 'single_choice',
          prompt: 'Plan?',
          choices: ['Free', 'Pro'],
        },
      ],
      targeting: {
        path_pattern: '/checkout/*',
        trigger_event: 'checkout_completed',
        sample_rate: 0.25,
        once_per_visitor: false,
        flag: { key: 'beta', value: 'test' },
      },
      created_at: '2026-11-20T00:00:00Z',
      updated_at: '2026-11-20T00:00:00Z',
    }
    const draft = draftFromSurvey(survey)
    expect(draft.samplePercent).toBe('25')
    expect(draft.flagValue).toBe('"test"')
    const body = draftToBody(draft)
    expect(body.questions).toEqual([
      {
        id: 'stars',
        kind: 'rating',
        prompt: 'Rate it',
        optional: true,
        scale: 10,
        lower_label: 'Bad',
      },
      {
        id: 'plan',
        kind: 'single_choice',
        prompt: 'Plan?',
        optional: false,
        choices: ['Free', 'Pro'],
      },
    ])
    expect(body.targeting).toEqual(survey.targeting)
    expect(describeTargeting(survey.targeting)).toBe(
      'On /checkout/*, after "checkout_completed", 25% of visitors, where beta = "test"'
    )
  })
})

describe('survey results helpers', () => {
  test('every status offers a way forward and never itself', () => {
    for (const status of ['draft', 'active', 'paused', 'completed'] as const) {
      const actions = statusActions(status)
      expect(actions.length).toBeGreaterThan(0)
      expect(actions.some((a) => a.to === status)).toBe(false)
    }
  })

  test('formats answers and percentages', () => {
    expect(formatAnswer(['Free', 'Pro'])).toBe('Free, Pro')
    expect(formatAnswer(9)).toBe('9')
    expect(formatAnswer(null)).toBe('')
    expect(percentOf(1, 3)).toBe(33)
    expect(percentOf(5, 0)).toBe(0)
  })
})
