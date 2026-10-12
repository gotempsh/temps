// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { useState } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'

import {
  confirmDeleteCopy,
  useConfirmDelete,
  type ConfirmDeleteCallbacks,
  type ConfirmDeleteController,
} from './confirm-delete'
import { ConfirmDeleteDialog } from './confirm-delete-dialog'

describe('confirmDeleteCopy', () => {
  test('names the target in the title and the description', () => {
    const copy = confirmDeleteCopy({
      resourceType: 'alert rule',
      resourceName: 'High error rate',
    })
    expect(copy.title).toBe('Delete alert rule “High error rate”?')
    expect(copy.description).toContain('“High error rate”')
    expect(copy.description).toContain('cannot be undone')
    expect(copy.confirmLabel).toBe('Delete')
  })

  test('falls back to the resource type when the target has no name', () => {
    const copy = confirmDeleteCopy({
      resourceType: 'email provider',
      resourceName: '  ',
    })
    expect(copy.title).toBe('Delete email provider?')
    expect(copy.description).toContain('this email provider')
  })

  test('shows a pending label while the request is in flight', () => {
    const copy = confirmDeleteCopy({
      resourceType: 'notification route',
      resourceName: 'Pager',
      isPending: true,
    })
    expect(copy.confirmLabel).toBe('Deleting…')
  })
})

describe('ConfirmDeleteDialog', () => {
  test('renders nothing until it is opened and never calls onConfirm', () => {
    const calls: string[] = []
    const markup = renderToStaticMarkup(
      <ConfirmDeleteDialog
        open={false}
        onOpenChange={() => calls.push('open-change')}
        resourceType="alert rule"
        resourceName="High error rate"
        onConfirm={() => calls.push('confirm')}
      />
    )
    expect(markup).not.toContain('High error rate')
    expect(markup).not.toContain('Delete')
    expect(calls).toEqual([])
  })
})

function last<T>(items: T[]): T | undefined {
  return items[items.length - 1]
}

interface Snapshot {
  target: unknown
  open: boolean
}

/**
 * Drives the real hook through a script of steps in one server render, using
 * render-phase state updates (supported by React's server renderer). This
 * keeps the test free of a DOM while still exercising actual `useState`.
 */
function runScript<T>(
  performDelete: (target: T, callbacks: ConfirmDeleteCallbacks) => void,
  script: Array<(controller: ConfirmDeleteController<T>) => void>
): Snapshot[] {
  const snapshots: Snapshot[] = []
  function Harness() {
    const [step, setStep] = useState(0)
    const controller = useConfirmDelete<T>(performDelete)
    snapshots.push({ target: controller.target, open: controller.open })
    if (step < script.length) {
      script[step](controller)
      setStep(step + 1)
    }
    return null
  }
  renderToStaticMarkup(<Harness />)
  return snapshots
}

describe('useConfirmDelete', () => {
  const rule = { id: 7, name: 'High error rate' }

  test('requesting a delete opens the dialog and sends nothing', () => {
    const sent: unknown[] = []
    const snapshots = runScript<typeof rule>(
      (target) => sent.push(target),
      [(c) => c.request(rule)]
    )
    expect(last(snapshots)).toEqual({ target: rule, open: true })
    expect(sent).toEqual([])
  })

  test('confirming sends exactly one delete for the requested item', () => {
    const sent: unknown[] = []
    runScript<typeof rule>(
      (target) => sent.push(target),
      [(c) => c.request(rule), (c) => c.confirm()]
    )
    expect(sent).toEqual([rule])
  })

  test('cancelling closes the dialog and sends nothing', () => {
    const sent: unknown[] = []
    const snapshots = runScript<typeof rule>(
      (target) => sent.push(target),
      [(c) => c.request(rule), (c) => c.cancel()]
    )
    expect(last(snapshots)).toEqual({ target: null, open: false })
    expect(sent).toEqual([])
  })

  test('confirm without a pending target is a no-op', () => {
    const sent: unknown[] = []
    runScript<typeof rule>((target) => sent.push(target), [(c) => c.confirm()])
    expect(sent).toEqual([])
  })

  test('a failed delete leaves the dialog open, a successful one closes it', () => {
    const failed = runScript<typeof rule>(() => {
      /* onSuccess never fires: the request failed */
    }, [(c) => c.request(rule), (c) => c.confirm()])
    expect(last(failed)).toEqual({ target: rule, open: true })

    const succeeded = runScript<typeof rule>(
      (_target, callbacks) => callbacks.onSuccess(),
      [(c) => c.request(rule), (c) => c.confirm()]
    )
    expect(last(succeeded)).toEqual({ target: null, open: false })
  })
})
