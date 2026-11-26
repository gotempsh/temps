// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter, useSearchParams } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import {
  assistantHref,
  lastChat,
  rememberLastChat,
  useRememberShownChat,
} from './assistant-entry'

installBrowserWindow()
beforeEach(() => localStorage.clear())
afterEach(cleanup)

test('with no chat on record, Ask opens a new one', () => {
  expect(lastChat()).toBeNull()
  expect(assistantHref()).toBe('/chat')
})

test('Ask reopens the chat last shown', () => {
  rememberLastChat('conv_1a2b')
  rememberLastChat('3f0c9d2e-7a41-4c1b-9a55-0d6f2b8e1c77')
  expect(assistantHref()).toBe(
    '/chat?chat=3f0c9d2e-7a41-4c1b-9a55-0d6f2b8e1c77'
  )
})

test('what is not a chat id is neither stored nor followed', () => {
  rememberLastChat('conv_1')
  rememberLastChat('')
  rememberLastChat('a b&next=/elsewhere')
  expect(lastChat()).toBe('conv_1')

  localStorage.setItem('temps:assistant:last-chat', '../settings?x=1')
  expect(lastChat()).toBeNull()
  expect(assistantHref()).toBe('/chat')
})

/** The assistant page, reduced to its address. */
function Page() {
  useRememberShownChat()
  const [, setSearchParams] = useSearchParams()
  return (
    <>
      <button onClick={() => setSearchParams({ chat: 'conv_2' })}>Send</button>
      <button onClick={() => setSearchParams({})}>New chat</button>
    </>
  )
}

test('the assistant page records the chat it shows, and keeps it over an empty new chat', () => {
  const view = render(
    <MemoryRouter initialEntries={['/chat?chat=conv_1']}>
      <Page />
    </MemoryRouter>
  )
  expect(assistantHref()).toBe('/chat?chat=conv_1')

  fireEvent.click(view.getByRole('button', { name: 'New chat' }))
  expect(assistantHref()).toBe('/chat?chat=conv_1')

  fireEvent.click(view.getByRole('button', { name: 'Send' }))
  expect(assistantHref()).toBe('/chat?chat=conv_2')
})
