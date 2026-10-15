// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import path from 'node:path'
import { cleanup, render } from '@testing-library/react'
import axe from 'axe-core'
import { installBrowserWindow } from '@/test/dom'
import { ProjectBadge, ProjectLegend } from './ProjectBadge-components'
import { PROJECT_COLORS } from './ProjectBadge-shared'

installBrowserWindow()
afterEach(cleanup)

/**
 * axe's `color-contrast` rule needs real layout, which the test DOM does not
 * have, so the contrast guarantee is checked arithmetically from the same
 * values the browser uses: the badge text is the `--foreground` token and its
 * surface is the project hue at 0x22 alpha over `--background`.
 */
const GLOBALS = readFileSync(
  path.resolve(import.meta.dir, '../../globals.css'),
  'utf8'
)

/** Achromatic `oklch(L 0 0)` token from a theme block, as a 0..1 sRGB channel. */
function neutralToken(block: ':root' | '.dark', name: string): number {
  const start = GLOBALS.indexOf(`\n${block} {`)
  const match = new RegExp(`--${name}:\\s*oklch\\(([\\d.]+) 0 0\\)`).exec(
    GLOBALS.slice(start)
  )
  if (!match)
    throw new Error(`--${name} is not an achromatic oklch in ${block}`)
  // For a neutral colour linear light is L^3; encode it back to sRGB.
  const linear = Number(match[1]) ** 3
  return linear <= 0.0031308
    ? linear * 12.92
    : 1.055 * linear ** (1 / 2.4) - 0.055
}

const toLinear = (c: number) =>
  c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4
const luminance = (rgb: [number, number, number]) =>
  0.2126 * toLinear(rgb[0]) +
  0.7152 * toLinear(rgb[1]) +
  0.0722 * toLinear(rgb[2])
const hex = (value: string): [number, number, number] => [
  parseInt(value.slice(1, 3), 16) / 255,
  parseInt(value.slice(3, 5), 16) / 255,
  parseInt(value.slice(5, 7), 16) / 255,
]
function contrast(a: number, b: number): number {
  const [hi, lo] = a > b ? [a, b] : [b, a]
  return (hi + 0.05) / (lo + 0.05)
}

describe('ProjectBadge', () => {
  test('names the project in the foreground colour, not in its own hue', () => {
    const view = render(<ProjectBadge projectId={1} name="Example API" />)
    const badge = view.getByTitle('Example API')
    expect(badge.className).toContain('text-foreground')
    // An inline `color` would override the token and bring back the pastel
    // text on a pastel tint that axe reported at ~2:1.
    expect(badge.getAttribute('style')).not.toMatch(/(^|;)\s*color:/)
    expect(badge.getAttribute('style')).toContain('background-color')
  })

  test.each(['.dark', ':root'] as const)(
    'keeps every palette hue at 4.5:1 or better in the %s theme',
    (block) => {
      const alpha = 0x22 / 255
      const background = neutralToken(block, 'background')
      const foreground = luminance(
        Array(3).fill(neutralToken(block, 'foreground')) as [
          number,
          number,
          number,
        ]
      )
      for (const color of PROJECT_COLORS) {
        const surface = hex(color).map(
          (channel) => channel * alpha + background * (1 - alpha)
        ) as [number, number, number]
        expect(contrast(foreground, luminance(surface))).toBeGreaterThanOrEqual(
          4.5
        )
      }
    }
  )

  test('the legend has no accessibility violations', async () => {
    const view = render(
      <ProjectLegend
        projects={[
          { project_id: 1, project_name: 'Example API' },
          { project_id: 2, project_name: 'Example worker' },
        ]}
      />
    )
    const result = await axe.run(view.container, {
      runOnly: {
        type: 'rule',
        values: ['aria-allowed-attr', 'aria-prohibited-attr', 'button-name'],
      },
    })
    expect(result.violations.map((v) => `${v.id}: ${v.nodes.length}`)).toEqual(
      []
    )
  })
})
