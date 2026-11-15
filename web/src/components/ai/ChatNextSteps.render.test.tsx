// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  NextStepsChips,
  NextStepsOffer,
  SuggestionSettingsMenu,
} from './ChatNextSteps'

const noop = () => {}

test('the offer asks first and gives all three answers', () => {
  const markup = renderToStaticMarkup(
    <NextStepsOffer onYes={noop} onAlways={noop} onNo={noop} />
  )
  expect(markup).toContain('Suggest next steps?')
  expect(markup).toContain('>Yes<')
  expect(markup).toContain('>Always<')
  expect(markup).toContain('>No<')
})

test('the offer says what each answer applies to', () => {
  const markup = renderToStaticMarkup(
    <NextStepsOffer onYes={noop} onAlways={noop} onNo={noop} />
  )
  expect(markup).toContain('title="Suggest after every answer in this chat"')
  expect(markup).toContain(
    'title="Suggest after every answer in all your chats, this one included"'
  )
  expect(markup).toContain('title="Not for this answer"')
})

test('the suggestions are labelled, and each one is a send button', () => {
  const markup = renderToStaticMarkup(
    <NextStepsChips
      suggestions={['Show the deployment logs', 'Add a custom domain']}
      onPick={noop}
    />
  )
  expect(markup).toContain('Suggested next steps')
  expect(markup).toContain('Show the deployment logs')
  expect(markup).toContain('Add a custom domain')
  expect(markup.match(/<button/g)?.length).toBe(2)
  expect(markup.match(/Send this as your next message/g)?.length).toBe(2)
  // Without the handlers there is no edit or hide control to mislead.
  expect(markup).not.toContain('Edit before sending')
  expect(markup).not.toContain('Hide suggestions')
})

test('each suggestion can be edited first, and the set can be hidden', () => {
  const markup = renderToStaticMarkup(
    <NextStepsChips
      suggestions={['Redeploy both projects', 'Read the logs']}
      onPick={noop}
      onEdit={noop}
      onDismiss={noop}
    />
  )
  expect(markup).toContain(
    'aria-label="Edit before sending: Redeploy both projects"'
  )
  expect(markup).toContain('aria-label="Edit before sending: Read the logs"')
  expect(markup).toContain('aria-label="Hide suggestions"')
  // Two send buttons, two edit buttons, one hide button.
  expect(markup.match(/<button/g)?.length).toBe(5)
})

test('a chat following the default says which default it follows', () => {
  const markup = renderToStaticMarkup(
    <SuggestionSettingsMenu
      settings={{
        available: true,
        mode: 'always',
        source: 'default',
        chat_mode: null,
        default_mode: 'always',
      }}
      defaultMode="always"
      busy={false}
      onChatMode={noop}
      onDefaultMode={noop}
    />
  )
  expect(markup).toContain('Use my default (always suggest)')
  expect(markup).toContain('My default for all chats')
  expect(markup).toContain('one small extra request')
  // Selected: "use my default" in this chat, "always" in the default.
  expect(markup.match(/aria-checked="true"/g)?.length).toBe(2)
})

test('a chat with its own setting marks it, and saving disables the options', () => {
  const markup = renderToStaticMarkup(
    <SuggestionSettingsMenu
      settings={{
        available: true,
        mode: 'off',
        source: 'chat',
        chat_mode: 'off',
        default_mode: 'ask',
      }}
      defaultMode="ask"
      busy={true}
      onChatMode={noop}
      onDefaultMode={noop}
    />
  )
  expect(markup).toContain('Use my default (ask after each answer)')
  expect(markup.match(/aria-checked="true"/g)?.length).toBe(2)
  expect(markup.match(/disabled=""/g)?.length).toBe(7)
})
