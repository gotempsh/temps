// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import {
  AiAssistantContext,
  type AiAssistantValue,
  type OpenOptions,
} from './AiAssistantContext-shared'
import { FixErrorWithAgentButton } from './FixErrorWithAgentButton'
import {
  fixErrorWithAgentOptions,
  offersFixInList,
} from './fix-error-with-agent'

installBrowserWindow()
afterEach(cleanup)

const error = {
  id: 42,
  title: 'payment provider did not respond within 10s',
  error_type: '*payments.ProviderTimeoutError',
  environment_name: 'production',
  project: { id: 3, slug: 'checkout-api', name: 'checkout-api' },
}

test('opens a project chat about this one error, in its project', () => {
  const opts = fixErrorWithAgentOptions(error)
  expect(opts.projectId).toBe(3)
  expect(opts.context?.contextType).toBe('project')
  expect(opts.context?.contextId).toBe('error-group-42')
  expect(opts.context?.projectSlug).toBe('checkout-api')
  expect(opts.context?.startPrompt).toContain(
    'error #42 in checkout-api (production)'
  )
  expect(opts.context?.startPrompt).toContain(
    'Error: *payments.ProviderTimeoutError: payment provider did not respond within 10s'
  )
})

test('does not repeat a type the title already starts with', () => {
  const opts = fixErrorWithAgentOptions({
    ...error,
    title: 'TypeError: x is undefined',
    error_type: 'TypeError',
    environment_name: null,
  })
  expect(opts.context?.startPrompt).toContain(
    'Error: TypeError: x is undefined'
  )
  expect(opts.context?.startPrompt).not.toContain('TypeError: TypeError')
  expect(opts.context?.startPrompt).toContain('error #42 in checkout-api and')
})

test('lists offer the fix only on errors still waiting for one', () => {
  expect(offersFixInList('unresolved')).toBe(true)
  expect(offersFixInList('resolved')).toBe(false)
  expect(offersFixInList('ignored')).toBe(false)
})

function Location() {
  return <p data-testid="path">{useLocation().pathname}</p>
}

test('opens Agents on the error without triggering the row it sits in', () => {
  const opened: OpenOptions[] = []
  let rowClicks = 0
  const value = {
    open: (o?: OpenOptions) => o && opened.push(o),
  } as unknown as AiAssistantValue
  const { getByRole, getByTestId } = render(
    <AiAssistantContext.Provider value={value}>
      <MemoryRouter initialEntries={['/errors']}>
        <Routes>
          <Route
            path="*"
            element={
              <>
                <div onClick={() => rowClicks++}>
                  <FixErrorWithAgentButton error={error} />
                </div>
                <Location />
              </>
            }
          />
        </Routes>
      </MemoryRouter>
    </AiAssistantContext.Provider>
  )
  fireEvent.click(getByRole('button', { name: /fix with agent/i }))
  expect(opened).toEqual([fixErrorWithAgentOptions(error)])
  expect(getByTestId('path').textContent).toBe('/chat')
  expect(rowClicks).toBe(0)
})
