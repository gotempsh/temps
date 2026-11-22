// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import path from 'node:path'

/**
 * Regression guard for WCAG 2.5.3 Label in Name (axe `label-content-name-
 * mismatch`): an `aria-label` replaces a control's visible text, so a voice
 * control user saying the words they can see matches nothing when the label
 * leaves them out. These two controls carried a shorter label than the text
 * they show; their content now names them.
 */

const SRC = path.resolve(import.meta.dir, '../..')
const read = (file: string) => readFileSync(path.join(SRC, file), 'utf8')

/** The opening tag of the first element in `src` that contains `marker`. */
function openingTagContaining(src: string, marker: string): string {
  const at = src.indexOf(marker)
  expect(at).toBeGreaterThan(-1)
  const start = src.lastIndexOf('<', at)
  const end = src.indexOf('>', at)
  return src.slice(start, end + 1)
}

test('the database type cards are named by their visible content', () => {
  const link = openingTagContaining(
    read('components/project/ProjectStorage.tsx'),
    'serviceCreateHref(provider.service_type, project.id)'
  )
  expect(link).not.toContain('aria-label')
})

test('the environment variables Filters button is named by its content', () => {
  const button = openingTagContaining(
    read('components/project/settings/EnvironmentVariablesSettings.tsx'),
    'aria-controls="variable-filters"'
  )
  expect(button).not.toContain('aria-label')
})
