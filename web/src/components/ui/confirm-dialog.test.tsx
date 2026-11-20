// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { ConfirmDialog } from './confirm-dialog'
import { Sheet, SheetContent, SheetDescription, SheetTitle } from './sheet'

installBrowserWindow()
afterEach(cleanup)

/** Largest arbitrary `z-[N]` / `z-N` utility on an element, 0 when absent. */
function zIndexOf(element: Element | null): number {
  const match = /(?:^|\s)z-(?:\[(\d+)\]|(\d+))(?:\s|$)/.exec(
    element?.getAttribute('class') ?? ''
  )
  return Number(match?.[1] ?? match?.[2] ?? 0)
}

test('a confirmation opened from a Sheet stacks above the sheet and can be clicked', async () => {
  const onConfirm = mock(() => {})
  render(
    <Sheet open>
      <SheetContent>
        <SheetTitle>Edit flag</SheetTitle>
        <SheetDescription>Flag settings</SheetDescription>
        <ConfirmDialog
          open
          onOpenChange={() => {}}
          title="Archive checkout-redesign?"
          description="This hides the flag."
          confirmLabel="Archive"
          onConfirm={onConfirm}
        />
      </SheetContent>
    </Sheet>
  )

  const sheetOverlay = document.body.querySelector(
    '[data-radix-dialog-overlay], [class*="inset-0"][class*="z-["]'
  )
  const sheet = document.body.querySelector('[role="dialog"]')
  const alert = document.body.querySelector('[role="alertdialog"]')
  expect(sheet).not.toBeNull()
  expect(alert).not.toBeNull()
  // Both layers are portalled to <body>, so the later/higher z-index wins:
  // the confirmation (content and its overlay) must outrank the sheet.
  expect(zIndexOf(alert)).toBeGreaterThan(zIndexOf(sheet))
  expect(zIndexOf(alert)).toBeGreaterThan(zIndexOf(sheetOverlay))
  const alertOverlay = alert?.previousElementSibling ?? null
  expect(zIndexOf(alertOverlay)).toBeGreaterThan(zIndexOf(sheet))

  await act(async () => {
    fireEvent.click(
      document.body.querySelector('[role="alertdialog"] button:last-of-type')!
    )
  })
  expect(onConfirm).toHaveBeenCalledTimes(1)
})
