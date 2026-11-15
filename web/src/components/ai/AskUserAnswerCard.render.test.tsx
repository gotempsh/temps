// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { AskUserAnswerCard } from './AskUserAnswerCard'

test('shows each question with the answer the user gave', () => {
  const markup = renderToStaticMarkup(
    <AskUserAnswerCard
      outcome={{
        status: 'answered',
        answers: [
          {
            question: 'Which database should I create?',
            header: 'Database',
            answer: 'PostgreSQL',
          },
          { question: 'What should it be called?', header: null, answer: '' },
        ],
      }}
    />
  )
  expect(markup).toContain('Your answers')
  expect(markup).toContain('Which database should I create?')
  expect(markup).toContain('>PostgreSQL<')
  expect(markup).toContain('Left blank')
})

test('says when a question went unanswered', () => {
  const markup = renderToStaticMarkup(
    <AskUserAnswerCard
      outcome={{ status: 'not_answered', reason: 'the turn was stopped' }}
    />
  )
  expect(markup).toContain('Question not answered')
  expect(markup).toContain('the turn was stopped')
})
