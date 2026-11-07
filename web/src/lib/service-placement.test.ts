// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  CONTROL_PLANE_PLACEMENT,
  placementNodeId,
  standalonePlacement,
} from './service-placement'

describe('standalonePlacement', () => {
  // The live failure: on a control-plane-profile server the form started
  // on "Control plane", so a MongoDB, Redis or S3 service created without
  // touching the picker was sent with no node and rejected.
  test('a control plane that runs no containers starts on the first worker', () => {
    const placement = standalonePlacement(null, false, [4, 7])
    expect(placement).toEqual({
      offerControlPlane: false,
      value: '4',
      noTarget: false,
    })
    expect(placementNodeId(placement.value)).toBe(4)
  })

  test('the control plane is never kept as a choice when it runs no containers', () => {
    expect(
      standalonePlacement(CONTROL_PLANE_PLACEMENT, false, [4]).value
    ).toBe('4')
  })

  test('a picked worker is kept', () => {
    expect(standalonePlacement('7', false, [4, 7]).value).toBe('7')
    expect(standalonePlacement('7', true, [4, 7]).value).toBe('7')
  })

  test('a worker that left the list falls back to the default', () => {
    expect(standalonePlacement('9', false, [4]).value).toBe('4')
    expect(standalonePlacement('9', true, [4]).value).toBe(
      CONTROL_PLANE_PLACEMENT
    )
  })

  test('no local workloads and no worker: nothing can run it', () => {
    expect(standalonePlacement(null, false, [])).toEqual({
      offerControlPlane: false,
      value: null,
      noTarget: true,
    })
  })

  test('a control plane that runs containers keeps the previous default', () => {
    for (const local of [true, undefined]) {
      const placement = standalonePlacement(null, local, [4])
      expect(placement.offerControlPlane).toBe(true)
      expect(placement.value).toBe(CONTROL_PLANE_PLACEMENT)
      expect(placementNodeId(placement.value)).toBeNull()
    }
  })
})
