// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from '@playwright/test'

for (const hasTraces of [false, true]) {
  test(`trace onboarding keeps explorer ${hasTraces ? 'visible for existing traces' : 'hidden for new projects'}`, async ({
    page,
  }) => {
    const { projects } = await (await page.request.get('/api/projects')).json()
    const project = projects[0]
    await page.route('**/otel/has-traces/*', (route) =>
      route.fulfill({ json: { has_traces: hasTraces } })
    )
    await page.goto(`/projects/${project.slug}/traces`)
    const setup = page.getByRole('heading', {
      name: 'Setup OpenTelemetry',
      exact: true,
    })
    const search = page.getByPlaceholder('Search by trace ID...')
    if (hasTraces) {
      await expect(search).toBeVisible()
      await expect(setup).not.toBeVisible()
    } else {
      await expect(setup).toBeVisible()
      await expect(search).toHaveCount(0)
      await expect(
        page.getByText('No traces found', { exact: true })
      ).toHaveCount(0)
      await page.screenshot({
        path: '/tmp/temps-traces-onboarding-only.png',
        fullPage: true,
      })
    }
  })
}
