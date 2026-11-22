// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  answeredQuestionPermissionIds,
  isAskUserTool,
  parseAskUserResult,
} from './ask-user'
import type { ChatPart } from './chat-message-parts'

const answered = JSON.stringify({
  status: 'answered',
  answers: [
    {
      question: 'Which database should I create?',
      header: 'Database',
      answer: 'PostgreSQL',
    },
    { question: 'What should it be called?', answer: '' },
  ],
  note: 'An empty answer means the user left that question blank.',
})

describe('isAskUserTool', () => {
  test('matches the plain and MCP-qualified names', () => {
    expect(isAskUserTool('temps_ask_user')).toBe(true)
    expect(isAskUserTool('mcp__temps-chat__temps_ask_user')).toBe(true)
    expect(isAskUserTool('temps')).toBe(false)
  })
})

describe('parseAskUserResult', () => {
  test('reads the saved answers', () => {
    expect(parseAskUserResult(answered)).toEqual({
      status: 'answered',
      answers: [
        {
          question: 'Which database should I create?',
          header: 'Database',
          answer: 'PostgreSQL',
        },
        { question: 'What should it be called?', header: null, answer: '' },
      ],
    })
  })

  test('reads an unanswered question', () => {
    expect(
      parseAskUserResult(
        JSON.stringify({ status: 'not_answered', reason: 'the turn ended' })
      )
    ).toEqual({ status: 'not_answered', reason: 'the turn ended' })
  })

  test('is null while open and for a validation message', () => {
    expect(parseAskUserResult(undefined)).toBeNull()
    expect(parseAskUserResult(null)).toBeNull()
    expect(
      parseAskUserResult('`temps_ask_user` takes 1 to 4 questions')
    ).toBeNull()
    expect(
      parseAskUserResult(JSON.stringify({ status: 'answered', answers: [] }))
    ).toBeNull()
  })
})

describe('answeredQuestionPermissionIds', () => {
  const question = (id: string, questions: string[]): ChatPart => ({
    type: 'permission',
    permission: {
      id,
      kind: 'question',
      tool_name: 'temps_ask_user',
      input: {
        questions: questions.map((q) => ({ question: q, options: [] })),
      },
    },
  })
  const tool = (result?: string): ChatPart => ({
    type: 'tool',
    tool: { id: 'call-1', name: 'temps_ask_user', arguments: '{}', result },
  })

  test('hides a question card once its answers are saved', () => {
    const parts = [
      tool(answered),
      question('p1', [
        'Which database should I create?',
        'What should it be called?',
      ]),
    ]
    expect([...answeredQuestionPermissionIds(parts)]).toEqual(['p1'])
  })

  test('keeps an open question card visible', () => {
    expect(
      answeredQuestionPermissionIds([
        tool(undefined),
        question('p1', ['Which database should I create?']),
      ]).size
    ).toBe(0)
  })

  test('a second, different question in the same turn stays visible', () => {
    const parts = [
      tool(answered),
      question('p1', [
        'Which database should I create?',
        'What should it be called?',
      ]),
      question('p2', ['Which region?']),
    ]
    expect([...answeredQuestionPermissionIds(parts)]).toEqual(['p1'])
  })
})
