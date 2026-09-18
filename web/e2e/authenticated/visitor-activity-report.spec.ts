// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

async function openSettings(page: Page) {
  if (
    !(await page
      .getByRole('textbox', {
        name: 'What do you want to understand?',
        exact: true,
      })
      .isVisible())
  ) {
    await page.getByText('Report settings', { exact: true }).click()
  }
}

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
              ai_provider: 'OpenRouter',
              ai_model: 'deepseek/test-model',
              has_recent_activity: true,
              selected_environment_id: 1,
              setup_url: '/settings/ai-providers',
              settings,
              settings_revision: saved ? 1 : 0,
              running: false,
              next_run_at: null,
              last_error: null,
              report: completed ? report : null,
              recent_runs: completed
                ? [
                    {
                      trigger: 'manual',
                      status: 'success',
                      environment_id: 1,
                      started_at: report.started_at,
                      completed_at: report.completed_at,
                      analyzed_visitors: 1,
                      skipped_visitors: 3,
                      model: 'test-model',
                      error: null,
                    },
                  ]
                : [],
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
        expect(request.min_sessions).toBe(2)
        expect(request.min_page_paths).toBe(2)
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
    await expect(page.getByRole('button', { name: 'Run now' })).toHaveCount(0)
    await expect(
      page.getByRole('textbox', {
        name: 'What do you want to understand?',
        exact: true,
      })
    ).toBeHidden()
    await expect(
      page.getByRole('button', { name: 'Enable daily reports' })
    ).toHaveCount(0)
    await expect(page.getByLabel('Public application URL')).toHaveCount(0)
    await expect(
      page.getByRole('combobox', { name: 'Environment', exact: true })
    ).toHaveCount(0)
    await expect(
      page.getByRole('list', { name: 'Activity report setup steps' })
    ).toHaveCount(0)
    await expect(
      page.getByText('OpenRouter · deepseek/test-model', { exact: true })
    ).toBeVisible()
    await expect(
      page.getByRole('switch', { name: /Share public pages/ })
    ).toHaveCount(0)
    await page
      .getByRole('button', { name: 'Analyze website', exact: true })
      .click()
    await expect(page.getByLabel('Website', { exact: true })).toBeHidden()
    await expect(
      page.getByText('Report settings', { exact: true })
    ).toBeHidden()
    await page.getByText('Why this goal?', { exact: true }).click()
    await expect(
      page.getByText('Your site has installation documentation.', {
        exact: true,
      })
    ).toBeVisible()
    await page
      .getByRole('button', { name: /Understand documentation readers/ })
      .click()
    await expect(
      page.getByRole('textbox', {
        name: 'What do you want to understand?',
        exact: true,
      })
    ).toHaveValue(
      'We provide application hosting. Installation docs describe setup.'
    )
    expect(saved).toBe(false)
    await expect(
      page.getByRole('switch', { name: /Share visitor activity/ })
    ).not.toBeChecked()
    await page
      .getByRole('textbox', {
        name: 'What do you want to understand?',
        exact: true,
      })
      .scrollIntoViewIfNeeded()
    await page.screenshot({
      path: `/tmp/temps-activity-onboarding-${width}.png`,
    })
    await expect(page.getByLabel('Category 1 name')).not.toBeVisible()
    await page.getByText('Advanced settings', { exact: true }).click()
    await page.getByLabel('Custom event property keys (optional)').fill('topic')
    await page.getByRole('switch', { name: /Share visitor activity/ }).check()
    await page.getByRole('button', { name: 'Preview my visitors' }).click()
    await expect(page.getByText(report.summary)).toBeVisible()
    expect(saved).toBe(false)
    expect(completed).toBe(false)
    await expect(page.getByRole('button', { name: 'Run now' })).toHaveCount(0)
    await expect(page.getByText('Categories', { exact: true })).toBeVisible()
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
    await expect(page.getByRole('button', { name: 'Run now' })).toBeEnabled()
    expect(settings.property_keys).toEqual(['topic'])
    expect(settings.share_activity_with_ai).toBe(true)
    expect(settings.daily_enabled).toBe(true)
    await expect(
      page.getByRole('heading', { name: 'Saved goal', exact: true })
    ).toBeVisible()
    await expect(
      page.getByText('Daily reports on', { exact: true })
    ).toBeVisible()
    await page.reload()
    await expect(
      page.getByRole('heading', { name: 'Saved goal', exact: true })
    ).toBeVisible()
    await expect(
      page.getByRole('button', { name: 'Analyze website', exact: true })
    ).toHaveCount(0)
    await expect(
      page.getByText('Understand documentation readers', { exact: true })
    ).toBeVisible()
    // Preview-generated settings must not change the operator's schedule choice.
    const dailySwitch = page.getByRole('switch', {
      name: 'Run automatically every 24 hours',
    })
    for (const dailyEnabled of [true, false]) {
      await openSettings(page)
      if (!(await dailySwitch.isVisible())) {
        await page.getByText('Advanced settings', { exact: true }).click()
      }
      await dailySwitch.setChecked(dailyEnabled)
      await page
        .getByLabel('What do you want to understand?', { exact: true })
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
    await page.getByRole('button', { name: 'Run now' }).click()
    await expect(page.getByText(report.summary)).toBeVisible()
    await expect(page.getByText(/Sampled report:/)).toBeVisible()
    const history = page.getByRole('region', { name: 'Recent runs' })
    await expect(history.getByText('Completed', { exact: true })).toBeVisible()
    await expect(
      history.getByText('1 analyzed · 3 skipped · test-model')
    ).toBeVisible()
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
    await openSettings(page)
    await page.getByRole('switch', { name: /Share visitor activity/ }).uncheck()
    await page
      .getByRole('button', { name: 'Save settings', exact: true })
      .click()
    await expect(page.getByRole('button', { name: 'Run now' })).toBeDisabled()
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
    page.getByRole('button', { name: 'Analyze website', exact: true })
  ).toBeDisabled()
  await openSettings(page)
  await expect(
    page.getByRole('textbox', {
      name: 'What do you want to understand?',
      exact: true,
    })
  ).toBeVisible()
  await expect(page.getByRole('button', { name: 'Run now' })).toHaveCount(0)
})

for (const width of [390, 1280]) {
  test(`empty activity hides preview and keeps manual runs available at ${width}px`, async ({
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
            ai_provider: 'OpenRouter',
            ai_model: 'deepseek/test-model',
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
    await page.route(
      `**/api/projects/${project.id}/analytics/activity/goals`,
      (route) =>
        route.fulfill({
          json: {
            model: 'test-model',
            pages_read: ['https://example.com'],
            goals: [
              {
                title: 'Documentation interest',
                goal: 'Understand documentation readers',
                rationale: 'The website has guides.',
                missing_signals: 'Track article reads.',
              },
              {
                title: 'Product evaluation',
                goal: 'Find visitors exploring pricing and comparing deployment options.',
                rationale: 'The website includes pricing and comparison pages.',
                missing_signals: 'Track trial starts.',
              },
              {
                title: 'Setup progress',
                goal: 'Understand where visitors get stuck while setting up their first project.',
                rationale: 'The documentation includes installation guides.',
                missing_signals: 'Track successful deployments.',
              },
            ],
          },
        })
    )
    await page.goto(`/projects/${project.slug}/analytics/activity`)
    await expect(
      page.getByRole('heading', { name: 'Saved goal', exact: true })
    ).toBeVisible()
    await expect(
      page.getByRole('button', { name: 'Analyze website', exact: true })
    ).toHaveCount(0)
    await page.getByRole('button', { name: 'Change goal', exact: true }).click()
    await expect(
      page.getByRole('textbox', {
        name: 'What do you want to understand?',
        exact: true,
      })
    ).toBeHidden()
    await page
      .getByRole('button', { name: 'Analyze website', exact: true })
      .click()
    await expect(
      page.getByText('Suggested goals', { exact: true })
    ).toBeVisible()
    await expect(
      page.getByRole('button', { name: /Documentation interest/ })
    ).toBeVisible()
    await expect(page.getByText('Recommended', { exact: true })).toHaveCount(1)
    const recommended = page
      .getByRole('article')
      .filter({ hasText: 'Recommended' })
    await expect(
      recommended.getByRole('heading', { name: 'Documentation interest' })
    ).toBeVisible()
    await expect(recommended.getByRole('button')).toHaveText(
      'Use recommended goal'
    )
    expect(previewCalls).toBe(0)
    await expect(page.getByLabel('Website', { exact: true })).toBeHidden()
    await expect(
      page.getByRole('heading', { name: 'Suggested goals' })
    ).toBeFocused()
    await page.screenshot({ path: `/tmp/temps-website-results-${width}.png` })
    await page.getByRole('button', { name: 'Change website' }).click()
    await expect(page.getByLabel('Website', { exact: true })).toBeVisible()
    await page
      .getByRole('button', { name: 'Analyze website', exact: true })
      .click()
    await page
      .getByRole('button', { name: /Choose Documentation interest/ })
      .click()
    await expect(
      page.getByLabel('What do you want to understand?', { exact: true })
    ).toBeVisible()
    await page.getByRole('button', { name: 'Change goal' }).click()
    await expect(
      page.getByRole('heading', { name: 'Suggested goals' })
    ).toBeVisible()
    await page
      .getByRole('button', {
        name: 'Choose Documentation interest',
        exact: true,
      })
      .click()
    await expect(
      page.getByText('No visitor activity in the last 24 hours.', {
        exact: true,
      })
    ).toBeVisible()
    await expect(
      page.getByRole('button', { name: 'Preview my visitors' })
    ).toHaveCount(0)
    await expect(page.getByRole('button', { name: 'Run now' })).toBeEnabled()
    await expect(
      page.getByRole('textbox', {
        name: 'What do you want to understand?',
        exact: true,
      })
    ).toBeEditable()
    await page
      .getByRole('textbox', {
        name: 'What do you want to understand?',
        exact: true,
      })
      .press('Control+Enter')
    expect(previewCalls).toBe(0)
    await page.screenshot({
      path: `/tmp/temps-empty-activity-${width}.png`,
      fullPage: true,
    })
    hasActivity = true
    await page.reload()
    await openSettings(page)
    await expect(
      page.getByRole('button', { name: 'Preview my visitors' })
    ).toBeEnabled()
    await expect(page.getByRole('button', { name: 'Run now' })).toBeEnabled()
    await expect(
      page.getByText('No visitor activity in the last 24 hours.', {
        exact: true,
      })
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
            ai_provider: 'OpenRouter',
            ai_model: 'deepseek/test-model',
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
      name: 'Environment',
    })
    const website = page.getByRole('combobox', {
      name: 'Website',
    })
    const url = page.getByRole('textbox', {
      name: 'Public application URL',
      exact: true,
    })
    await expect(
      page.getByRole('button', { name: 'Analyze website', exact: true })
    ).toBeVisible()
    await expect(environment).toBeHidden()
    await page.screenshot({ path: `/tmp/temps-website-first-${width}.png` })
    await openSettings(page)
    await page.getByText('Advanced settings', { exact: true }).click()
    await expect(environment).toContainText('Production')
    await expect(url).toHaveCount(0)
    await expect(website).toContainText('https://app-production.example.com')
    await environment.click()
    await page.getByRole('option', { name: 'Staging', exact: true }).click()
    await expect(url).toHaveCount(0)
    await expect(website).toContainText('https://app-staging.example.com')
    await expect(
      page.getByText('No visitor activity in the last 24 hours.', {
        exact: true,
      })
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
      await openSettings(page)
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
    await page.getByRole('button', { name: 'Change goal', exact: true }).click()
    await expect(url).toHaveValue('https://docs.example.com')
    await website.click()
    await page
      .getByRole('option', { name: 'www.example.com', exact: true })
      .click()
    await expect(url).toHaveCount(0)
    await expect(website).toContainText('www.example.com')
    await save()
    expect(settings.source_url).toBeNull()
    expect(settings.source_domain).toBe('www.example.com')
    await page.reload()
    await page.getByRole('button', { name: 'Change goal', exact: true }).click()
    await expect(url).toHaveCount(0)
    await expect(website).toContainText('www.example.com')
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
    await page.getByRole('button', { name: 'Change goal', exact: true }).click()
    await expect(url).toHaveCount(0)
    await expect(website).toContainText(
      'https://renamed-production.example.com'
    )
    await website.scrollIntoViewIfNeeded()
    await page.screenshot({
      path: `/tmp/temps-environment-onboarding-${width}.png`,
    })
  })
}

test('website analysis shows the actual failure and allows retry', async ({
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
          configured: true,
          selected_environment_id: 1,
          settings_revision: 0,
          has_recent_activity: false,
          running: false,
          report: null,
          setup_url: '/settings/ai-providers',
          settings: {
            environment_id: 1,
            application_context: '',
            categories: [],
            property_keys: [],
            daily_enabled: false,
            share_activity_with_ai: false,
          },
        },
      })
  )
  await page.route(
    '**/api/projects/*/analytics/activity/goals',
    async (route) => {
      await route.fulfill({
        status: 409,
        contentType: 'application/problem+json',
        json: {
          title: 'Visitor activity analysis',
          detail:
            'Please wait 30 seconds before requesting more goal suggestions.',
        },
      })
    }
  )
  await page.goto(`/projects/${project.slug}/analytics/activity`)
  await page.getByLabel('Website', { exact: true }).click()
  await page
    .getByRole('option', { name: 'Use another URL', exact: true })
    .click()
  await page.getByLabel('Public application URL').fill('https://example.com')
  await page
    .getByRole('button', { name: 'Analyze website', exact: true })
    .click()
  await expect(
    page.getByRole('alert').filter({
      hasText:
        'Please wait 30 seconds before requesting more goal suggestions.',
    })
  ).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Analyze website', exact: true })
  ).toBeEnabled()
})

test('manual runs record skipped work and keep history after reload', async ({
  page,
}) => {
  const { projects } = await (await page.request.get('/api/projects')).json()
  test.skip(!projects[0], 'Requires a test project')
  const project = projects[0]
  let ran = false
  let revision = 1
  let settings = {
    environment_id: 1,
    goal_title: 'Documentation engagement',
    application_context: 'Understand documentation readers',
    categories: [{ name: 'Learning', description: 'Reading guides' }],
    property_keys: [],
    daily_enabled: false,
    share_activity_with_ai: true,
    min_sessions: 2,
    min_page_paths: 2,
  }
  const skipped = {
    trigger: 'manual',
    status: 'skipped',
    environment_id: 1,
    started_at: '2026-09-18T10:00:00Z',
    completed_at: '2026-09-18T10:00:01Z',
    analyzed_visitors: 0,
    skipped_visitors: 4,
    skipped_low_activity: 3,
    skipped_unchanged: 1,
    model: null,
    error: null,
  }
  await page.route(
    `**/api/projects/${project.id}/analytics/activity`,
    async (route) => {
      if (route.request().method() === 'PUT') {
        settings = route.request().postDataJSON()
        revision += 1
        return route.fulfill({ status: 204 })
      }
      return route.fulfill({
        json: {
          configured: true,
          selected_environment_id: 1,
          settings_revision: revision,
          settings,
          has_recent_activity: false,
          running: false,
          report: {
            started_at: '2026-09-16T10:00:00Z',
            completed_at: '2026-09-16T10:00:05Z',
            window_start: '2026-09-15T10:00:00Z',
            window_end: '2026-09-16T10:00:00Z',
            settings_revision: 1,
            categories: settings.categories,
            model: 'test-model',
            summary: 'Earlier visitors read the documentation.',
            sampled: false,
            events_considered: 4,
            visitors: [],
          },
          setup_url: '/settings/ai-providers',
          recent_runs: [
            ...(ran ? [skipped] : []),
            {
              ...skipped,
              trigger: 'scheduled',
              status: 'failed',
              started_at: '2026-09-17T10:00:00Z',
              skipped_visitors: 0,
              skipped_low_activity: 0,
              skipped_unchanged: 0,
              error: 'The AI provider was unavailable.',
            },
          ],
        },
      })
    }
  )
  await page.route(
    `**/api/projects/${project.id}/analytics/activity/run`,
    async (route) => {
      ran = true
      await route.fulfill({ json: {} })
    }
  )
  await page.goto(`/projects/${project.slug}/analytics/activity`)
  await expect(
    page.getByText('Documentation engagement', { exact: true })
  ).toBeVisible()
  await expect(
    page.getByText('Understand documentation readers', { exact: true })
  ).not.toBeVisible()
  await page.getByText('Documentation engagement', { exact: true }).click()
  await expect(
    page.getByText('Understand documentation readers', { exact: true })
  ).toBeVisible()
  await expect(
    page.getByRole('heading', { name: 'Latest report', exact: true })
  ).toBeVisible()
  await expect(
    page.getByText('The latest run failed. Showing the last available report.')
  ).toBeVisible()
  const history = page.getByRole('region', { name: 'Recent runs' })
  await expect(history.getByText('Failed', { exact: true })).toBeVisible()
  await expect(
    history.getByText('The AI provider was unavailable.')
  ).toBeVisible()
  await page.getByRole('button', { name: 'Run now', exact: true }).click()
  await expect(
    history.getByText('Nothing new to analyze', { exact: true })
  ).toBeVisible()
  await expect(
    history.getByText('0 analyzed · 4 skipped', { exact: true })
  ).toBeVisible()
  await expect(
    history.getByText('3 below threshold · 1 unchanged')
  ).toBeVisible()
  await expect(
    page.getByText(
      'The latest run found nothing new to analyze. Showing the last available report.'
    )
  ).toBeVisible()
  await expect(
    page.getByText('Last 24 hours · Up to 20 visitors', { exact: true })
  ).toHaveCount(0)
  await page.reload()
  await expect(
    history.getByText('Nothing new to analyze', { exact: true })
  ).toBeVisible()
  await openSettings(page)
  const dailySwitch = page.getByRole('switch', {
    name: 'Run automatically every 24 hours',
  })
  await expect(dailySwitch).toBeVisible()
  await dailySwitch.check()
  await page.getByText('Advanced settings', { exact: true }).click()
  await page.getByLabel('Minimum sessions', { exact: true }).fill('3')
  await page.getByLabel('Minimum distinct pages', { exact: true }).fill('4')
  await page.getByRole('button', { name: 'Save settings', exact: true }).click()
  await expect(
    page.getByText('Activity report setup saved', { exact: true }).first()
  ).toBeVisible()
  expect(settings.daily_enabled).toBe(true)
  expect(settings.goal_title).toBe('Documentation engagement')
  expect(settings.min_sessions).toBe(3)
  expect(settings.min_page_paths).toBe(4)
})
