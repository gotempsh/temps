// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { AiAssistantProvider, useAiAssistant } from './AiAssistantContext'
import type { AiAssistantValue } from './AiAssistantContext-shared'

installBrowserWindow()
afterEach(cleanup)

function capture() {
  const ref: { current: AiAssistantValue | null } = { current: null }
  function Probe() {
    ref.current = useAiAssistant()
    return null
  }
  render(
    <AiAssistantProvider>
      <Probe />
    </AiAssistantProvider>
  )
  return ref
}

const context = {
  contextType: 'project',
  contextId: 'c-1',
  title: 'Weekly health summary',
  startPrompt: 'Summarise the last 7 days',
}

test('handing a chat to the full page does not open the overlay', () => {
  const assistant = capture()
  act(() => assistant.current?.openPage({ projectId: 7, context }))
  expect(assistant.current?.isOpen).toBe(false)
  expect(assistant.current?.projectId).toBe(7)
  expect(assistant.current?.initialContext).toEqual(context)
})

test('the page forgets the handed-over chat once it has taken it', () => {
  const assistant = capture()
  act(() => assistant.current?.openPage({ projectId: 7, context }))
  act(() => assistant.current?.clearInitialContext())
  expect(assistant.current?.initialContext).toBeUndefined()
  expect(assistant.current?.isOpen).toBe(false)
})

test('the overlay still opens when a page asks for it', () => {
  const assistant = capture()
  act(() => assistant.current?.open({ projectId: 7, context }))
  expect(assistant.current?.isOpen).toBe(true)
})
