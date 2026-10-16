// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { useOpenSession } from './useOpenSession'

installBrowserWindow()
afterEach(cleanup)

function Probe({ trigger }: { trigger: unknown }) {
  return <p data-testid="session">{useOpenSession(trigger)}</p>
}

function sessionOf(view: ReturnType<typeof render>): string {
  return view.getByTestId('session').textContent ?? ''
}

test('advances on every opening and holds still while closing', () => {
  const view = render(<Probe trigger={false} />)
  expect(sessionOf(view)).toBe('0')

  view.rerender(<Probe trigger />)
  expect(sessionOf(view)).toBe('1')

  // Closing must not remount the form while the dialog animates shut.
  view.rerender(<Probe trigger={false} />)
  expect(sessionOf(view)).toBe('1')

  view.rerender(<Probe trigger />)
  expect(sessionOf(view)).toBe('2')
})

test('advances when the record the dialog was opened for changes', () => {
  const first = { id: 1 }
  const second = { id: 2 }
  const view = render(<Probe trigger={first} />)
  expect(sessionOf(view)).toBe('0')

  view.rerender(<Probe trigger={first} />)
  expect(sessionOf(view)).toBe('0')

  view.rerender(<Probe trigger={second} />)
  expect(sessionOf(view)).toBe('1')

  view.rerender(<Probe trigger={null} />)
  expect(sessionOf(view)).toBe('1')

  // Reopening the very same record still starts a fresh form.
  view.rerender(<Probe trigger={second} />)
  expect(sessionOf(view)).toBe('2')
})
