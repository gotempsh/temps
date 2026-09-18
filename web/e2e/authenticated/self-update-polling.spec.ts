// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'
import type {
  SelfUpdateStatus,
  UpdateCapabilityResponse,
} from '../../src/api/client/types.gen'

// Intercept every update request: exercising this dialog must NEVER replace
// the binary or restart the real instance serving the browser test.
for (const outcome of [
  { status: 'succeeded', title: 'Update complete' },
  { status: 'failed', title: 'Update failed' },
  { status: 'installed_pending_restart', title: 'Update installed' },
] satisfies { status: SelfUpdateStatus; title: string }[]) {
  test(`self update keeps its ${outcome.status} result and stops polling`, async ({
    page,
  }) => {
    const errors: string[] = []
    page.on('pageerror', (error) => errors.push(error.message))
    let started = false
    let completed = false
    let capabilityRequests = 0
    let starts = 0
    await page.route('**/api/settings/update', async (route) => {
      if (route.request().method() === 'POST') {
        starts++
        started = true
        await route.fulfill({ json: { estimated_restart_secs: 45 } })
        return
      }
      capabilityRequests++
      const capability: UpdateCapabilityResponse = {
        allowed: true,
        can_apply: true,
        binary_path: '/test/temps',
        channel: 'stable',
        channel_is_pinned: false,
        current_version: 'v0.1.0',
        manual_command: 'temps upgrade',
        phase: started && !completed ? 'downloading' : 'idle',
        restart_mode:
          outcome.status === 'installed_pending_restart'
            ? 'manual'
            : 'automatic',
        supervisor: 'none',
        last_attempt: started
          ? {
              from_version: 'v0.1.0',
              to_version: 'v0.1.1',
              started_at: '2026-09-17T12:00:00Z',
              status: completed ? outcome.status : 'pending',
              error: outcome.status === 'failed' ? 'Checksum mismatch' : null,
            }
          : {
              from_version: 'v0.0.9',
              to_version: 'v0.1.0',
              started_at: '2026-09-16T12:00:00Z',
              status: 'succeeded',
            },
      }
      await route.fulfill({ json: capability })
    })
    await page.route('**/api/settings/update-status', (route) =>
      route.fulfill({
        json: {
          update_available: !completed,
          current_version: 'v0.1.0',
          latest_version: 'v0.1.1',
          docs_url: 'https://temps.sh/docs',
        },
      })
    )
    await page.goto('/settings/version')
    await page
      .getByRole('button', { name: 'Update now', exact: true })
      .last()
      .click()
    const dialog = page.getByRole('alertdialog')
    await dialog
      .getByRole('button', {
        name:
          outcome.status === 'installed_pending_restart'
            ? 'Install update'
            : 'Update and restart',
        exact: true,
      })
      .click()
    await expect(dialog.getByRole('heading')).toHaveText('Updating temps')
    completed = true
    await expect(dialog.getByRole('heading')).toHaveText(outcome.title)
    // Let the single result-driven cache invalidation settle, then observe
    // longer than two polling intervals. An unstable effect callback loops.
    await page.waitForTimeout(1000)
    const settledRequests = capabilityRequests
    await page.waitForTimeout(4500)
    expect(capabilityRequests).toBe(settledRequests)
    await expect(dialog.getByRole('heading')).toHaveText(outcome.title)
    if (outcome.status === 'failed') {
      await expect(dialog).toContainText('Checksum mismatch')
    }
    await dialog.getByRole('button', { name: 'Close', exact: true }).click()
    await expect(dialog).toBeHidden()
    expect(starts).toBe(1)
    expect(errors).toEqual([])
  })
}
