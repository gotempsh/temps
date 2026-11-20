// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { renderToStaticMarkup } from 'react-dom/server'
import type { ChatMessage } from './chat-message-parts'
import { TranscriptRow } from './DebugChatPanel-components'

const noop = () => {}

function row(message: ChatMessage, streaming = false) {
  return renderToStaticMarkup(
    <TranscriptRow
      message={message}
      streaming={streaming}
      activityLabel="Thinking"
      publicId="chat_1"
      userScoped
      fromProject={false}
      onFix={noop}
      onPermissionResolved={noop}
    />
  )
}

test('a settled user row renders its text and opts into off-screen skipping', () => {
  const markup = row({ role: 'user', content: 'list my projects' })
  expect(markup).toContain('list my projects')
  expect(markup).toContain('chat-transcript-row')
})

test('a settled assistant row renders its prose and opts into off-screen skipping', () => {
  const markup = row({ role: 'assistant', content: 'You have two projects.' })
  expect(markup).toContain('You have two projects.')
  expect(markup).toContain('chat-transcript-row')
})

test('the streaming row never skips rendering, because its height changes every token', () => {
  const markup = row({ role: 'assistant', content: 'Working on it' }, true)
  expect(markup).toContain('Working on it')
  expect(markup).not.toContain('chat-transcript-row')
})

test('an assistant turn with nothing to show leaves no empty bubble', () => {
  expect(row({ role: 'assistant', content: '' })).toBe('')
})

test('a streaming assistant turn with nothing yet still shows its activity', () => {
  expect(row({ role: 'assistant', content: '' }, true)).toContain(
    'data-testid="transcript-row-assistant"'
  )
})

test('the row is memoized, so a streamed token does not re-render earlier rows', () => {
  // `memo` wraps the component in an object tagged with its own $$typeof.
  expect(
    String((TranscriptRow as unknown as { $$typeof: symbol }).$$typeof)
  ).toContain('react.memo')
})

test('the skip-rendering rule exists in the stylesheet', () => {
  const css = readFileSync(
    new URL('../../globals.css', import.meta.url),
    'utf8'
  )
  expect(css).toMatch(
    /\.chat-transcript-row\s*{[^}]*content-visibility:\s*auto[^}]*contain-intrinsic-size:\s*auto/
  )
})
