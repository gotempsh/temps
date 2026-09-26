// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

/**
 * The telemetry storage page alerts only while delivery to Temps Cloud is
 * failing *now*. Spans lost to a failure that is already over are history: a
 * dated row under Storage history, never a red banner telling a recovered
 * instance to re-enroll.
 */

const project = {
  id: 72,
  name: 'Delivery demo',
  slug: 'delivery-demo',
  main_branch: 'main',
  directory: '.',
  source_type: 'docker_image',
  project_type: 'application',
  attack_mode: false,
  deployment_config: {},
  created_at: 0,
  updated_at: 0,
}

const pastGap = {
  first_span_at: '2026-09-24T09:00:00.000Z',
  last_span_at: '2026-09-24T15:00:00.000Z',
  undelivered_spans: 48_213,
  gave_up_at: '2026-09-24T17:03:27.000Z',
  last_error: 'Credential rejected by the backend — re-enroll this instance',
}

const baseSettings = {
  project_id: project.id,
  fidelity: 'queryable',
  attribute_allowlist: [],
  write_mode: 'cloud',
  effective_write_mode: 'cloud',
  analytics_write_mode: 'local',
  cloud_write_mode_available: true,
  queued_spans: 0,
  dead_lettered_spans: pastGap.undelivered_spans,
  last_dead_letter_error: pastGap.last_error,
  last_dead_letter_at: pastGap.gave_up_at,
  delivery_failing: false,
  retrying_spans: 0,
  delivery_gaps: [pastGap],
  gap_windows: [],
  intervals: [
    {
      mode: 'cloud',
      effective_from: '2026-09-02T16:30:36.000Z',
      reason: 'operator',
      message: 'Changed by an operator in project settings.',
    },
  ],
}

async function mockProject(page: Page, settings: object) {
  await page.route('**/api/projects?*', (route) =>
    route.fulfill({ json: { projects: [project], total: 1 } })
  )
  await page.route('**/api/projects', (route) =>
    route.fulfill({ json: { projects: [project], total: 1 } })
  )
  await page.route(`**/api/projects/by-slug/${project.slug}`, (route) =>
    route.fulfill({ json: project })
  )
  await page.route(`**/api/projects/${project.id}/environments`, (route) =>
    route.fulfill({ json: [] })
  )
  await page.route(
    `**/api/otel/cloud-telemetry/projects/${project.id}`,
    (route) => route.fulfill({ json: settings })
  )
}

const failingTitle = 'Delivery to Temps Cloud is failing'
const historyHeading = 'Spans never delivered to Temps Cloud'

test('a recovered instance shows the loss as history, not as an alert', async ({
  page,
}) => {
  await mockProject(page, baseSettings)
  await page.goto(`/projects/${project.slug}/settings/telemetry`)

  await expect(page.getByText(historyHeading)).toBeVisible()
  await expect(page.getByText(/48,213 spans from/)).toBeVisible()
  await expect(page.getByText(pastGap.last_error)).toBeVisible()

  await expect(page.getByText(failingTitle)).toHaveCount(0)
  await expect(page.getByText(/were never delivered/)).toHaveCount(0)
  await expect(
    page.getByRole('link', { name: 'Open Temps Cloud settings' })
  ).toHaveCount(0)
  await page.getByText(historyHeading).scrollIntoViewIfNeeded()
  await page.screenshot({
    path: '/tmp/cloud-delivery-recovered.png',
    fullPage: true,
  })
})

test('a failing delivery alerts and links straight to re-enrollment', async ({
  page,
}) => {
  await mockProject(page, {
    ...baseSettings,
    queued_spans: 1_200,
    delivery_failing: true,
    retrying_spans: 1_200,
    delivery_failing_since: '2026-09-26T08:00:00.000Z',
    delivery_failure_error:
      'Credential rejected by the backend — re-enroll this instance',
    delivery_failure_action:
      "Temps Cloud rejected this instance's credential. Re-enroll the instance in Temps Cloud settings; the spans still being retried are delivered once it is accepted again.",
    delivery_failure_setup_path: '/settings/cloud',
  })
  await page.goto(`/projects/${project.slug}/settings/telemetry`)

  await expect(page.getByText(failingTitle)).toBeVisible()
  await expect(
    page.getByText(/1,200 spans are waiting to be delivered/)
  ).toBeVisible()
  await expect(
    page.getByRole('link', { name: 'Open Temps Cloud settings' })
  ).toHaveAttribute('href', '/settings/cloud')
  // The failing alert already accounts for the queue; a second "waiting"
  // notice for the same spans would read as two separate problems.
  await expect(page.getByText(/waiting to reach Temps/)).toHaveCount(0)
  // Earlier losses stay in history alongside the live alert.
  await expect(page.getByText(historyHeading)).toBeVisible()
  await page.screenshot({
    path: '/tmp/cloud-delivery-failing.png',
    fullPage: true,
  })
})

test('a project that never lost a span shows neither', async ({ page }) => {
  await mockProject(page, {
    ...baseSettings,
    dead_lettered_spans: 0,
    last_dead_letter_error: undefined,
    last_dead_letter_at: undefined,
    delivery_gaps: [],
  })
  await page.goto(`/projects/${project.slug}/settings/telemetry`)

  await expect(page.getByText('Storage history')).toBeVisible()
  await expect(page.getByText(failingTitle)).toHaveCount(0)
  await expect(page.getByText(historyHeading)).toHaveCount(0)
})
