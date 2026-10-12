// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

// The repo has no DOM test environment, so this pins the structural fix as a
// source scan (same precedent as `ui/destructive-actions-usage.test.ts`).
//
// Regression: the row's DropdownMenuContent is portaled, but React still
// bubbles its clicks through the component tree to the row Card's `onClick`
// (navigate to the webhook detail page). Choosing "Delete" therefore opened
// the confirmation and, in the same click, navigated away and unmounted it.
describe('WebhooksSettings row menu', () => {
  const source = readFileSync(
    join(import.meta.dir, 'WebhooksSettings.tsx'),
    'utf8'
  )

  test('menu clicks do not bubble to the row navigation handler', () => {
    const content = source.match(/<DropdownMenuContent[\s\S]*?\n\s*>/)
    expect(content).not.toBeNull()
    expect(content?.[0]).toContain('onClick={(e) => e.stopPropagation()}')
  })

  test('the row card still navigates to the detail page on click', () => {
    expect(source).toContain('/settings/webhooks/${webhook.id}')
  })
})
