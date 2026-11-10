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
      chosenUnavailable: false,
    })
    expect(placementNodeId(placement.value)).toBe(4)
  })

  test('a picked control plane that turns out to run no containers is not swapped for a worker', () => {
    // Picked while the capability was still loading.
    const placement = standalonePlacement(CONTROL_PLANE_PLACEMENT, false, [4])
    expect(placement.value).toBeNull()
    expect(placement.chosenUnavailable).toBe(true)
  })

  test('a picked worker is kept', () => {
    expect(standalonePlacement('7', false, [4, 7]).value).toBe('7')
    expect(standalonePlacement('7', true, [4, 7]).value).toBe('7')
  })

  // A worker the user picked went offline or started draining, and the
  // list refreshed: submitting must not create the service on the control
  // plane or on another worker instead.
  test('a picked worker that left the list is not replaced by another machine', () => {
    for (const local of [false, true, undefined]) {
      const placement = standalonePlacement('9', local, [4])
      expect(placement.value).toBeNull()
      expect(placement.chosenUnavailable).toBe(true)
      expect(placement.noTarget).toBe(false)
    }
  })

  test('picking again after the choice left clears the warning', () => {
    expect(standalonePlacement('4', false, [4])).toEqual({
      offerControlPlane: false,
      value: '4',
      noTarget: false,
      chosenUnavailable: false,
    })
  })

  test('no local workloads and no worker: nothing can run it', () => {
    expect(standalonePlacement(null, false, [])).toEqual({
      offerControlPlane: false,
      value: null,
      noTarget: true,
      chosenUnavailable: false,
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
