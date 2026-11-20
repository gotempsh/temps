// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, it } from 'bun:test'
import { act, cleanup, renderHook } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'

import { baselineStep, useDraftBaseline } from './draft-baseline'

installBrowserWindow()
afterEach(cleanup)

const base = { revision: 'r1', key: JSON.stringify({ v: 1 }), fields: { v: 1 } }

describe('baselineStep', () => {
  it('advances when the fields the draft writes did not change', () => {
    expect(baselineStep(base, base.key, { v: 1 }, () => true)).toBe('advance')
  })

  it('advances when the draft already equals the new stored fields', () => {
    const draft = { v: 2 }
    const differs = (fields: { v: number }) => fields.v !== draft.v
    expect(
      baselineStep(base, JSON.stringify({ v: 2 }), { v: 2 }, differs)
    ).toBe('advance')
  })

  it('reseeds a clean draft when the fields changed', () => {
    const draft = { v: 1 }
    const differs = (fields: { v: number }) => fields.v !== draft.v
    expect(
      baselineStep(base, JSON.stringify({ v: 3 }), { v: 3 }, differs)
    ).toBe('reseed')
  })

  it('keeps unsaved edits and the old revision when the fields changed under them', () => {
    const draft = { v: 2 }
    const differs = (fields: { v: number }) => fields.v !== draft.v
    expect(
      baselineStep(base, JSON.stringify({ v: 3 }), { v: 3 }, differs)
    ).toBe('keep')
  })
})

describe('useDraftBaseline', () => {
  interface Props {
    revision: string
    stored: number
    draft: string
    editing: boolean
  }
  function setup(initial: Props) {
    const reseeded: number[] = []
    const hook = renderHook(
      (props: Props) =>
        useDraftBaseline(
          props.revision,
          props.stored,
          (stored) => props.draft !== String(stored),
          (stored) => reseeded.push(stored),
          props.editing
        ),
      { initialProps: initial }
    )
    return { hook, reseeded }
  }

  it('keeps the old revision under unsaved edits', () => {
    const { hook, reseeded } = setup({
      revision: 'r1',
      stored: 1,
      draft: '5',
      editing: true,
    })
    hook.rerender({ revision: 'r2', stored: 2, draft: '5', editing: true })
    expect(hook.result.current[0]).toBe('r1')
    expect(reseeded).toEqual([])
  })

  it('adopts the revision of its own save even when the server normalised the value', () => {
    // The draft says "2.0"; the server stored 2.
    const { hook, reseeded } = setup({
      revision: 'r1',
      stored: 1,
      draft: '2.0',
      editing: true,
    })
    act(() => hook.result.current[1]())
    hook.rerender({ revision: 'r2', stored: 2, draft: '2.0', editing: true })
    expect(hook.result.current[0]).toBe('r2')
    expect(reseeded).toEqual([2])
    // Later edits are protected again.
    hook.rerender({ revision: 'r3', stored: 3, draft: '7', editing: true })
    expect(hook.result.current[0]).toBe('r2')
  })

  it('a closed editor takes every fetched revision', () => {
    const { hook, reseeded } = setup({
      revision: 'r1',
      stored: 1,
      draft: '2.0',
      editing: false,
    })
    hook.rerender({ revision: 'r2', stored: 2, draft: '2.0', editing: false })
    expect(hook.result.current[0]).toBe('r2')
    expect(reseeded).toEqual([2])
  })
})
