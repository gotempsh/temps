// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

for (const width of [390, 1280]) {
  test(`activity report configures, categorizes and preserves evidence at ${width}px`, async ({
    page,
  }) => {
    const { projects } = await (await page.request.get('/api/projects')).json()
    test.skip(!projects[0], 'Requires a test project')
    const project = projects[0]
    await page.setViewportSize({ width, height: 1000 })
    let settings = {
      application_context: '',
      categories: [
        { name: 'Learning', description: 'Reading educational content.' },
      ],
      property_keys: [] as string[],
      daily_enabled: false,
      share_activity_with_ai: false,
    }
    const report = {
      started_at: '2026-09-16T10:00:00Z',
      completed_at: '2026-09-16T10:00:05Z',
      window_start: '2026-09-15T10:00:00Z',
      window_end: '2026-09-16T10:00:00Z',
      settings_revision: 1,
      categories: settings.categories,
      model: 'test-model',
      summary: 'Readers explored installation documentation.',
      sampled: true,
      events_considered: 500,
      visitors: [
        {
          visitor_id: 73,
          categories: ['Learning'],
          explanation: 'Read the installation guide.',
          evidence: [
            {
              reference: 1,
              timestamp: '2026-09-16T09:00:00Z',
              path: '/docs/install',
              title: 'Installation',
              event: 'pageview',
              properties: [{ key: 'topic', value: 'installation' }],
            },
          ],
        },
      ],
    }
    let saved = false
    let completed = false
    await page.route(
      `**/api/projects/${project.id}/analytics/activity`,
      async (route) => {
        if (route.request().method() === 'PUT') {
          settings = route.request().postDataJSON()
          saved = true
          await route.fulfill({ status: 204 })
        } else {
          await route.fulfill({
            json: {
              configured: true,
              setup_url: '/settings/ai-providers',
              settings,
              settings_revision: saved ? 1 : 0,
              running: false,
              next_run_at: null,
              last_error: null,
              report: completed ? report : null,
            },
          })
        }
      }
    )
    await page.route(
      `**/api/projects/${project.id}/analytics/activity/run`,
      async (route) => {
        completed = true
        await route.fulfill({ json: report })
      }
    )
    let previewCalls = 0
    await page.route(
      `**/api/projects/${project.id}/analytics/activity/preview`,
      async (route) => {
        const request = route.request().postDataJSON()
        expect(request.share_activity_with_ai).toBe(true)
        expect(request.goal).toContain('application hosting')
        expect(request.property_keys).toEqual(['topic'])
        previewCalls += 1
        if (previewCalls === 2) {
          await route.fulfill({
            status: 502,
            json: { detail: 'The provider could not create a preview.' },
          })
          return
        }
        await route.fulfill({
          json: {
            settings: {
              ...settings,
              application_context: request.goal,
              categories: report.categories,
              property_keys: request.property_keys,
              share_activity_with_ai: true,
              daily_enabled: false,
            },
            report: { ...report, settings_revision: 0 },
          },
        })
      }
    )
    await page.route(`**/api/projects/${project.id}/environments`, (route) =>
      route.fulfill({
        json: [
          {
            id: 1,
            name: 'Production',
            slug: 'production',
            main_url: 'https://example.com/',
            current_deployment_id: 10,
          },
        ],
      })
    )
    await page.route(
      `**/api/projects/${project.id}/analytics/activity/goals`,
      async (route) => {
        expect(route.request().postDataJSON()).toEqual({
          url: 'https://example.com/',
          share_with_ai: true,
        })
        await route.fulfill({
          json: {
            model: 'test-model',
            pages_read: ['https://example.com/'],
            goals: [
              {
                title: 'Understand documentation readers',
                goal: 'We provide application hosting. Installation docs describe setup.',
                rationale: 'Your site has installation documentation.',
                missing_signals: 'Track successful setup completion.',
              },
            ],
          },
        })
      }
    )
    await page.goto(`/projects/${project.slug}/analytics/activity`)
    await expect(
      page.getByRole('heading', { name: 'Activity report', exact: true })
    ).toBeVisible()
    if (width === 1280)
      await expect(
        page.getByRole('link', { name: 'Activity report', exact: true })
      ).toHaveAttribute('aria-current', 'page')
    await expect(
      page.getByRole('button', { name: 'Run saved settings' })
    ).toBeDisabled()
    await expect(page.getByLabel('Public application URL')).toHaveValue(
      'https://example.com/'
    )
    await page
      .getByRole('switch', { name: /Allow sending public page content/ })
      .check()
    await page
      .getByRole('button', { name: 'Suggest goals from my app', exact: true })
      .click()
    await expect(
      page.getByText('Why this fits: Your site has installation documentation.')
    ).toBeVisible()
    await page
      .getByRole('button', { name: /Understand documentation readers/ })
      .click()
    await expect(
      page.getByRole('textbox', {
        name: 'Your application and goal',
        exact: true,
      })
    ).toHaveValue(
      'We provide application hosting. Installation docs describe setup.'
    )
    expect(saved).toBe(false)
    await expect(
      page.getByRole('switch', { name: /Allow analysis/ })
    ).not.toBeChecked()
    await page
      .getByRole('textbox', { name: 'Your application and goal', exact: true })
      .scrollIntoViewIfNeeded()
    await page.screenshot({
      path: `/tmp/temps-activity-onboarding-${width}.png`,
    })
    await expect(page.getByLabel('Category 1 name')).not.toBeVisible()
    await page.getByText('Advanced settings', { exact: true }).click()
    await page.getByLabel('Custom event property keys (optional)').fill('topic')
    await page.getByRole('switch', { name: /Allow analysis/ }).check()
    await page.getByRole('button', { name: 'Preview my visitors' }).click()
    await expect(page.getByText(report.summary)).toBeVisible()
    expect(saved).toBe(false)
    expect(completed).toBe(false)
    await expect(
      page.getByRole('button', { name: 'Run saved settings' })
    ).toBeDisabled()
    await expect(page.getByText('Categories for this setup')).toBeVisible()
    await page.getByRole('button', { name: 'Preview my visitors' }).click()
    await expect(
      page.getByText(/Your saved setup has not changed/)
    ).toBeVisible()
    expect(saved).toBe(false)
    await page.getByRole('button', { name: 'Preview my visitors' }).click()
    await expect(page.getByText(report.summary)).toBeVisible()
    await page
      .getByRole('button', { name: 'Enable daily reports', exact: true })
      .click()
    await expect(
      page.getByRole('button', { name: 'Run saved settings' })
    ).toBeEnabled()
    expect(settings.property_keys).toEqual(['topic'])
    expect(settings.share_activity_with_ai).toBe(true)
    expect(settings.daily_enabled).toBe(true)
    // Preview-generated settings must not change the operator's schedule choice.
    const dailySwitch = page.getByRole('switch', {
      name: 'Run automatically every 24 hours',
    })
    for (const dailyEnabled of [true, false]) {
      if (!(await dailySwitch.isVisible())) {
        await page.getByText('Advanced settings', { exact: true }).click()
      }
      await dailySwitch.setChecked(dailyEnabled)
      await page
        .getByLabel('Your application and goal', { exact: true })
        .fill(
          'We provide application hosting. Understand documentation readers and their next steps.'
        )
      await page.getByRole('button', { name: 'Preview my visitors' }).click()
      await expect(page.getByText(report.summary)).toBeVisible()
      await expect(dailySwitch).toBeChecked({ checked: dailyEnabled })
      const persisted = page.waitForResponse(
        (response) =>
          response.url().endsWith(`/analytics/activity`) &&
          response.request().method() === 'PUT'
      )
      await page
        .getByRole('button', { name: 'Save settings', exact: true })
        .click()
      expect((await persisted).request().postDataJSON().daily_enabled).toBe(
        dailyEnabled
      )
      expect(settings.daily_enabled).toBe(dailyEnabled)
    }
    await page.getByRole('button', { name: 'Run saved settings' }).click()
    await expect(page.getByText(report.summary)).toBeVisible()
    await expect(page.getByText(/Sampled report:/)).toBeVisible()
    await page
      .getByRole('button', { name: 'Learning (1)', exact: true })
      .click()
    await expect(
      page.getByRole('link', { name: 'Visitor #73' })
    ).toHaveAttribute('href', `/projects/${project.slug}/analytics/visitors/73`)
    await page.getByText('Supporting activity (1)').click()
    await expect(page.getByText(/topic: installation/)).toBeVisible()
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth
      )
    ).toBe(true)
    await page.screenshot({
      path: `/tmp/temps-activity-report-${width}.png`,
      fullPage: true,
    })

    // Revocation must be saveable, and must prevent future runs.
    await page.getByRole('switch', { name: /Allow analysis/ }).uncheck()
    await page
      .getByRole('button', { name: 'Save settings', exact: true })
      .click()
    await expect(
      page.getByRole('button', { name: 'Run saved settings' })
    ).toBeDisabled()
    expect(settings.share_activity_with_ai).toBe(false)
    expect(settings.daily_enabled).toBe(false)
  })
}

test('activity analysis is discoverable before an AI provider is configured', async ({
  page,
}) => {
  const { projects } = await (await page.request.get('/api/projects')).json()
  test.skip(!projects[0], 'Requires a test project')
  const project = projects[0]
  await page.route(
    `**/api/projects/${project.id}/analytics/activity`,
    (route) =>
      route.fulfill({
        json: {
          configured: false,
          setup_url: '/settings/ai-providers',
          settings_revision: 0,
          running: false,
          next_run_at: null,
          last_error: null,
          report: null,
          settings: {
            application_context: '',
            categories: [{ name: 'Learning', description: 'Reading' }],
            property_keys: [],
            daily_enabled: false,
            share_activity_with_ai: false,
          },
        },
      })
  )
  await page.goto(`/projects/${project.slug}/analytics/activity`)
  await expect(
    page.getByRole('link', { name: 'Configure AI provider', exact: true })
  ).toHaveAttribute('href', '/settings/ai-providers')
  await expect(
    page.getByRole('textbox', {
      name: 'Your application and goal',
      exact: true,
    })
  ).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Run saved settings' })
  ).toBeDisabled()
})
