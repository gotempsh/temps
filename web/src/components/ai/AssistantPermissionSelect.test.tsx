// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render as mount } from '@testing-library/react'
import { renderToStaticMarkup } from 'react-dom/server'
import { installBrowserWindow } from '@/test/dom'
import { AssistantPermissionSelect } from './AssistantPermissionSelect'
import { ASSISTANT_PERMISSION_CHOICES } from './assistant-state'

installBrowserWindow()
afterEach(cleanup)

const render = (permissionModeId: string | null, disabled = false) =>
  renderToStaticMarkup(
    <AssistantPermissionSelect
      permissionModeId={permissionModeId}
      disabled={disabled}
      onChange={() => {}}
    />
  )

test('offers exactly two modes: ask, and full permission', () => {
  expect(ASSISTANT_PERMISSION_CHOICES).toEqual([
    { id: 'confirm-actions', label: 'Ask' },
    { id: 'auto', label: 'Full permission' },
  ])
})

test('is a select named for what it sets, not a switch', () => {
  const markup = render('auto')
  expect(markup).toContain('role="combobox"')
  expect(markup).toContain('aria-label="Permission mode"')
  expect(markup).not.toContain('role="switch"')
})

test('shows the mode the chat is in', () => {
  for (const [mode, label] of [
    ['confirm-actions', 'Ask'],
    ['auto', 'Full permission'],
  ]) {
    const view = mount(
      <AssistantPermissionSelect permissionModeId={mode} onChange={() => {}} />
    )
    expect(view.getByRole('combobox').textContent).toBe(label)
    view.unmount()
  }
})

test('says deletions always ask, so full permission is not read as "no safety"', () => {
  expect(render('auto')).toContain('Deletions always ask')
})

test('can be disabled while a turn is running', () => {
  expect(render('auto', true)).toContain('disabled=""')
})
