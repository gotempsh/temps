// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import {
  OVERLAY_LAYERS,
  overlayLayerIndex,
  type OverlayLayer,
} from './overlay-layers'

const read = (file: string) => readFileSync(join(import.meta.dir, file), 'utf8')

describe('overlay layer tiers', () => {
  test('every tier is a literal Tailwind arbitrary z-index class', () => {
    for (const layer of Object.keys(OVERLAY_LAYERS) as OverlayLayer[]) {
      expect(OVERLAY_LAYERS[layer]).toMatch(/^z-\[\d+\]$/)
      expect(Number.isFinite(overlayLayerIndex(layer))).toBe(true)
    }
  })

  test('overlays opened from another overlay sit above it', () => {
    const order: OverlayLayer[] = [
      'sheetOverlay',
      'sheetContent',
      'dialogOverlay',
      'dialogContent',
      'alertDialogOverlay',
      'alertDialogContent',
      'popover',
      'tooltip',
    ]
    const indexes = order.map(overlayLayerIndex)
    for (let i = 1; i < indexes.length; i += 1) {
      expect(indexes[i]).toBeGreaterThan(indexes[i - 1])
    }
  })

  test('a modal overlay sits below its own content', () => {
    expect(overlayLayerIndex('sheetOverlay')).toBeLessThan(
      overlayLayerIndex('sheetContent')
    )
    expect(overlayLayerIndex('dialogOverlay')).toBeLessThan(
      overlayLayerIndex('dialogContent')
    )
    expect(overlayLayerIndex('alertDialogOverlay')).toBeLessThan(
      overlayLayerIndex('alertDialogContent')
    )
  })

  test('tiers stay above in-page chrome and the product tour', () => {
    expect(overlayLayerIndex('sheetOverlay')).toBeGreaterThan(100)
  })
})

describe('ui components use the shared tiers', () => {
  const expectations: Array<[string, OverlayLayer[]]> = [
    ['sheet.tsx', ['sheetOverlay', 'sheetContent']],
    ['dialog.tsx', ['dialogOverlay', 'dialogContent']],
    ['alert-dialog.tsx', ['alertDialogOverlay', 'alertDialogContent']],
    ['popover.tsx', ['popover']],
    ['select.tsx', ['popover']],
    ['dropdown-menu.tsx', ['popover']],
    ['tooltip.tsx', ['tooltip']],
  ]

  for (const [file, layers] of expectations) {
    test(`${file} references its tier and hardcodes no z-index`, () => {
      const source = read(file)
      for (const layer of layers) {
        expect(source).toContain(`OVERLAY_LAYERS.${layer}`)
      }
      expect(source).not.toMatch(/\bz-(\[\d+\]|\d+)\b/)
    })
  }
})
