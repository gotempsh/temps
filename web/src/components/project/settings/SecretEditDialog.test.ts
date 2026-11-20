// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

// There is no DOM test environment in the repo, so this pins the structure as
// a source scan (same precedent as `ui/destructive-actions-usage.test.ts`).
//
// Regression: the dialog stays mounted, and only the Dialog's own
// `onOpenChange` reset the mutation. The Cancel button called the parent's
// `onOpenChange` directly, so a failed save's error was shown again the next
// time the dialog opened.
describe('SecretEditDialog', () => {
  const source = readFileSync(
    join(import.meta.dir, 'SecretEditDialog.tsx'),
    'utf8'
  )

  test('every close path goes through the handler that resets the mutation', () => {
    expect(source).toContain('onOpenChange={handleOpenChange}')
    expect(source).toContain('onClick={() => handleOpenChange(false)}')
    expect(source).toContain('updateMutation.reset()')
  })

  test('nothing closes the dialog behind the handler', () => {
    // Success closes it after the save has settled, which is fine; any other
    // direct call would skip the reset again.
    const direct = source.match(/onClick=\{\(\) => onOpenChange\(/g) ?? []
    expect(direct).toHaveLength(0)
  })
})
