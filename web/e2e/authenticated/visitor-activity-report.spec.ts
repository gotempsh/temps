// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

const environmentFixtures = [
  {
    id: 1,
    name: 'Production',
    slug: 'production',
    main_url: 'https://app-production.example.com',
    current_deployment_id: 10,
  },
  {
    id: 2,
    name: 'Staging',
    slug: 'staging',
    main_url: 'https://app-staging.example.com',
    current_deployment_id: 20,
  },
]
test.beforeEach(async ({ page }) => {
  await page.route('**/api/projects/*/environments', (route) =>
    route.fulfill({ json: environmentFixtures })
  )
  await page.route('**/api/projects/*/environments/*/domains', (route) =>
    route.fulfill({
      json: [
        {
          id: 3,
          environment_id: 1,
          domain: 'www.example.com',
          url: 'https://www.example.com',
          created_at: 0,
        },
      ],
    })
  )
})

for (const width of [390, 1280]) {
  test(`activity report configures, categorizes and preserves evidence at ${width}px`, async ({
    page,
  }) => {
    const { projects } = await (await page.request.get('/api/projects')).json()
    test.skip(!projects[0], 'Requires a test project')
    const project = projects[0]
    await page.setViewportSize({ width, height: 1000 })
    let settings = {
      environment_id: 1,
      source_url: null as string | null,
      source_domain: null as string | null,
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
              has_recent_activity: true,
              selected_environment_id: 1,
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
          environment_id: 1,
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
        .getByRole('button', { name: /^(Save settings|Save setup)$/ })
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
          has_recent_activity: true,
          selected_environment_id: 1,
          setup_url: '/settings/ai-providers',
          settings_revision: 0,
          running: false,
          next_run_at: null,
          last_error: null,
          report: null,
          settings: {
            environment_id: 1,
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

for (const width of [390, 1280]) {
  test(`empty activity explains the wait and hides analysis actions at ${width}px`, async ({
    page,
  }) => {
    const { projects } = await (await page.request.get('/api/projects')).json()
    test.skip(!projects[0], 'Requires a test project')
    const project = projects[0]
    await page.setViewportSize({ width, height: 1000 })
    let hasActivity = false
    let previewCalls = 0
    await page.route(
      `**/api/projects/${project.id}/analytics/activity`,
      (route) =>
        route.fulfill({
          json: {
            configured: true,
            has_recent_activity: hasActivity,
            selected_environment_id: 1,
            setup_url: '/settings/ai-providers',
            settings_revision: 1,
            running: false,
            next_run_at: null,
            last_error: null,
            report: null,
            settings: {
              environment_id: 1,
              application_context: 'Understand readers of our documentation',
              categories: [
                { name: 'Learning', description: 'Reading documentation' },
              ],
              property_keys: [],
              daily_enabled: false,
              share_activity_with_ai: true,
            },
          },
        })
    )
    await page.route(
      `**/api/projects/${project.id}/analytics/activity/preview`,
      (route) => {
        previewCalls += 1
        return route.fulfill({ status: 500 })
      }
    )
    await page.goto(`/projects/${project.slug}/analytics/activity`)
    await expect(
      page.getByText(
        'No tracked visitor activity in this environment in the last 24 hours.',
        {
          exact: true,
        }
      )
    ).toBeVisible()
    await expect(
      page.getByRole('button', { name: 'Preview my visitors' })
    ).toHaveCount(0)
    await expect(
      page.getByRole('button', { name: 'Run saved settings' })
    ).toHaveCount(0)
    await expect(
      page.getByRole('textbox', {
        name: 'Your application and goal',
        exact: true,
      })
    ).toBeEditable()
    await page
      .getByRole('textbox', { name: 'Your application and goal', exact: true })
      .press('Control+Enter')
    expect(previewCalls).toBe(0)
    await page.screenshot({
      path: `/tmp/temps-empty-activity-${width}.png`,
      fullPage: true,
    })
    hasActivity = true
    await page.reload()
    await expect(
      page.getByRole('button', { name: 'Preview my visitors' })
    ).toBeEnabled()
    await expect(
      page.getByRole('button', { name: 'Run saved settings' })
    ).toBeEnabled()
    await expect(
      page.getByText(
        'No tracked visitor activity in this environment in the last 24 hours.',
        {
          exact: true,
        }
      )
    ).toHaveCount(0)
  })
}

for (const width of [390, 1280]) {
  test(`environment scope and website choices persist at ${width}px`, async ({
    page,
  }) => {
    const { projects } = await (await page.request.get('/api/projects')).json()
    test.skip(!projects[0], 'Requires a test project')
    const project = projects[0]
    await page.setViewportSize({ width, height: 1000 })
    let settings = {
      environment_id: 1,
      source_url: null as string | null,
      source_domain: null as string | null,
      application_context: 'Understand readers of our documentation',
      categories: [{ name: 'Learning', description: 'Reading documentation' }],
      property_keys: [],
      daily_enabled: false,
      share_activity_with_ai: true,
    }
    let revision = 0
    await page.route(
      /\/api\/projects\/[^/]+\/analytics\/activity(?:\?.*)?$/,
      async (route) => {
        if (route.request().method() === 'PUT') {
          settings = route.request().postDataJSON()
          revision += 1
          return route.fulfill({ status: 204 })
        }
        const id = Number(
          new URL(route.request().url()).searchParams.get('environment_id') ??
            settings.environment_id
        )
        await route.fulfill({
          json: {
            configured: true,
            has_recent_activity: id === 1,
            selected_environment_id: id,
            setup_url: '/settings/ai-providers',
            settings,
            settings_revision: revision,
            running: false,
            next_run_at: null,
            last_error: null,
            report: null,
          },
        })
      }
    )
    await page.goto(`/projects/${project.slug}/analytics/activity`)
    const environment = page.getByRole('combobox', {
      name: 'Analyze activity from',
    })
    const website = page.getByRole('combobox', {
      name: 'Website used to suggest goals',
    })
    const url = page.getByRole('textbox', {
      name: 'Public application URL',
      exact: true,
    })
    await expect(environment).toContainText('Production')
    await expect(url).toHaveValue('https://app-production.example.com')
    await environment.click()
    await page.getByRole('option', { name: 'Staging', exact: true }).click()
    await expect(url).toHaveValue('https://app-staging.example.com')
    await expect(
      page.getByText(
        'No tracked visitor activity in this environment in the last 24 hours.',
        { exact: true }
      )
    ).toBeVisible()
    await expect(
      page.getByRole('button', { name: 'Preview my visitors' })
    ).toHaveCount(0)
    await website.click()
    await page
      .getByRole('option', { name: 'Use another URL', exact: true })
      .click()
    await url.fill('https://docs.example.com')
    await environment.click()
    await page.getByRole('option', { name: 'Production', exact: true }).click()
    await expect(url).toHaveValue('https://docs.example.com')
    await expect(
      page.getByRole('button', { name: 'Preview my visitors' })
    ).toBeEnabled()
    const save = async () => {
      const response = page.waitForResponse(
        (r) =>
          r.url().endsWith('/analytics/activity') &&
          r.request().method() === 'PUT'
      )
      await page
        .getByRole('button', { name: /^(Save settings|Save setup)$/ })
        .click()
      await response
      await expect(
        page.getByText('Activity report setup saved', { exact: true }).first()
      ).toBeVisible()
    }
    await save()
    expect(settings.environment_id).toBe(1)
    expect(settings.source_url).toBe('https://docs.example.com')
    await page.reload()
    await expect(url).toHaveValue('https://docs.example.com')
    await website.click()
    await page
      .getByRole('option', { name: 'www.example.com', exact: true })
      .click()
    await expect(url).toHaveValue('https://www.example.com')
    await save()
    expect(settings.source_url).toBeNull()
    expect(settings.source_domain).toBe('www.example.com')
    await page.reload()
    await expect(url).toHaveValue('https://www.example.com')
    await website.click()
    await page.getByRole('option', { name: /Temps subdomain/ }).click()
    await save()
    expect(settings.source_url).toBeNull()
    expect(settings.source_domain).toBeNull()
    await page.route('**/api/projects/*/environments', (route) =>
      route.fulfill({
        json: [
          {
            ...environmentFixtures[0],
            main_url: 'https://renamed-production.example.com',
          },
          environmentFixtures[1],
        ],
      })
    )
    await page.reload()
    await expect(url).toHaveValue('https://renamed-production.example.com')
    await environment.scrollIntoViewIfNeeded()
    await page.screenshot({
      path: `/tmp/temps-environment-onboarding-${width}.png`,
    })
  })
}
