// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { ConfirmDeleteDialog } from './confirm-delete-dialog'

installBrowserWindow()
afterEach(cleanup)

function dialog(): HTMLElement {
  const el = document.body.querySelector<HTMLElement>('[role="alertdialog"]')
  if (!el) throw new Error('no alertdialog rendered')
  return el
}

function button(label: string): HTMLButtonElement {
  const found = Array.from(dialog().querySelectorAll('button')).find((b) =>
    b.textContent?.includes(label)
  )
  if (!found) throw new Error(`no button "${label}"`)
  return found as HTMLButtonElement
}

test('names the resource type and the specific resource in the title and body', () => {
  render(
    <ConfirmDeleteDialog
      open
      onOpenChange={() => {}}
      resourceType="email provider"
      resourceName="Postmark"
      consequences="Domains that rely on it will stop sending."
      onConfirm={() => {}}
    />
  )
  expect(dialog().textContent).toContain('Delete email provider Postmark?')
  expect(dialog().textContent).toContain('Domains that rely on it')
  expect(dialog().textContent).toContain('This cannot be undone.')
})

test('sends nothing until confirmed, and cancel does not confirm', async () => {
  const onConfirm = mock(() => {})
  const onOpenChange = mock((_open: boolean) => {})
  render(
    <ConfirmDeleteDialog
      open
      onOpenChange={onOpenChange}
      resourceType="funnel"
      resourceName="Checkout"
      onConfirm={onConfirm}
    />
  )
  expect(onConfirm).not.toHaveBeenCalled()
  await act(async () => {
    fireEvent.click(button('Cancel'))
  })
  expect(onConfirm).not.toHaveBeenCalled()
  expect(onOpenChange).toHaveBeenCalledWith(false)

  await act(async () => {
    fireEvent.click(button('Delete'))
  })
  expect(onConfirm).toHaveBeenCalledTimes(1)
})

test('never renders a nameless title', () => {
  render(
    <ConfirmDeleteDialog
      open
      onOpenChange={() => {}}
      resourceType="domain"
      resourceName="   "
      onConfirm={() => {}}
    />
  )
  expect(dialog().textContent).toContain('Delete domain this item?')
  expect(dialog().textContent).not.toContain('Delete domain ?')
})

test('requireTypedName keeps the button disabled until the name is typed', async () => {
  const onConfirm = mock(() => {})
  render(
    <ConfirmDeleteDialog
      open
      onOpenChange={() => {}}
      resourceType="project"
      resourceName="my-app"
      requireTypedName
      onConfirm={onConfirm}
    />
  )
  expect(button('Delete').disabled).toBe(true)
  const input = dialog().querySelector('input') as HTMLInputElement
  await act(async () => {
    fireEvent.change(input, { target: { value: 'my-ap' } })
  })
  expect(button('Delete').disabled).toBe(true)
  await act(async () => {
    fireEvent.change(input, { target: { value: 'my-app' } })
  })
  expect(button('Delete').disabled).toBe(false)
  await act(async () => {
    fireEvent.click(button('Delete'))
  })
  expect(onConfirm).toHaveBeenCalledTimes(1)
})

test('while pending the button and cancel are disabled so it cannot fire twice', () => {
  render(
    <ConfirmDeleteDialog
      open
      onOpenChange={() => {}}
      resourceType="API key"
      resourceName="ci"
      isPending
      onConfirm={() => {}}
    />
  )
  expect(button('Cancel').disabled).toBe(true)
  expect(button('Working').disabled).toBe(true)
})
