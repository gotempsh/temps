// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { OrphanBackupBadge } from './OrphanBackupBadge'

installBrowserWindow()
afterEach(cleanup)

test('focusing the orphan badge explains why the backup appears', async () => {
  const view = render(<OrphanBackupBadge />)
  const badge = view.getByText('orphan')
  expect(badge.tabIndex).toBe(0)
  fireEvent.focus(badge)
  expect((await view.findByRole('tooltip')).textContent).toContain(
    'has no backup record on this Temps server'
  )
})

test('hovering the badge tells the operator where to restore it', async () => {
  const view = render(<OrphanBackupBadge />)
  fireEvent.pointerMove(view.getByText('orphan'), { pointerType: 'mouse' })
  expect((await view.findByRole('tooltip')).textContent).toContain(
    'open the destination database’s Restore page'
  )
})
