// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'

import { baselineStep } from './draft-baseline'

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
