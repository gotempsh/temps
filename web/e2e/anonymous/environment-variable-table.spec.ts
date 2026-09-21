// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'
import type { HttpCheckView, SaveHttpCheck } from '../../src/api/client'

for (const width of [1440, 390]) {
  test(`environment variable table at ${width}px preserves secrets and explains checks`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 1000 })
    const reveals: string[] = []
    let checks: HttpCheckView[] = []
    let saved: SaveHttpCheck | undefined
    const errors: string[] = []
    page.on('pageerror', (error) => errors.push(error.message))
    const environment = {
      id: 1,
      name: 'production',
      slug: 'production',
      project_id: 1,
      domains: [],
      status: 'running',
    }
    const project = {
      id: 1,
      name: 'Example app',
      slug: 'example-app',
      preset: 'docker-compose',
      directory: '.',
      main_branch: 'main',
      source_type: 'git',
      is_public_repo: true,
      git_url: 'https://github.com/example/app',
      repo_owner: 'example',
      repo_name: 'app',
      environments: [environment],
      created_at: 1,
      updated_at: 1,
    }
    await page.route('**/api/**', async (route) => {
      const path = new URL(route.request().url()).pathname.replace(/^\/api/, '')
      let body: unknown = []
      if (path === '/projects/1/http-checks') {
        if (route.request().method() === 'POST') {
          saved = route.request().postDataJSON() as SaveHttpCheck
          const check: HttpCheckView = {
            id: 1,
            project_id: 1,
            env_var_id: saved.env_var_id,
            name: saved.name,
            automatic_provider: null,
            enabled: true,
            interval_seconds: saved.interval_seconds ?? 86400,
            next_check_at: '2026-09-22T00:00:00Z',
            last_checked_at: null,
            result: null,
          }
          checks = [check]
          body = check
        } else
          body = {
            items: checks,
            total: checks.length,
            page: 1,
            page_size: 100,
          }
      } else if (path === '/projects/1/env-vars/2/history') {
        const query = new URL(route.request().url()).searchParams
        const historyPage = Number(query.get('page') ?? 1)
        expect(query.get('page_size')).toBe('15')
        body = {
          items: Array.from(
            { length: historyPage === 1 ? 15 : 1 },
            (_, index) => ({
              id: (historyPage - 1) * 15 + index + 1,
              kind: historyPage === 1 ? 'value_changed' : 'created',
              details: {},
              created_at: '2026-09-21T00:00:00Z',
            })
          ),
          total: 16,
          page: historyPage,
          page_size: 15,
        }
      } else if (path === '/projects/1/http-checks/presets') {
        body = [
          'airtable',
          'anthropic',
          'digitalocean',
          'doppler',
          'github',
          'gitlab',
          'groq',
          'openai',
        ].map((id) => ({
          id,
          name: id === 'github' ? 'GitHub' : id === 'gitlab' ? 'GitLab' : id,
          description:
            id === 'gitlab'
              ? 'Checks expiration. Confirm the GitLab host.'
              : 'Checks account access.',
          automatic: id !== 'gitlab',
          documentation_url: 'https://example.com/docs',
          spec: {
            url:
              id === 'github'
                ? 'https://api.github.com/user'
                : `https://api.${id}.example/account`,
            method: 'get',
            headers: {},
            credential_header: 'Authorization',
            credential_prefix: 'Bearer ',
            accepted_statuses: [200],
            expiration: null,
            numeric_rules: [],
          },
        }))
      } else if (path === '/projects/1/http-checks/capabilities') {
        body = {
          detection_rule_count: 221,
          alerts_configured: false,
          alerts_setup_path: '/settings/notifications',
        }
      } else if (path === '/projects/1/env-vars/2/detect') {
        body = {
          env_var_id: 2,
          detection_rule_count: 221,
          candidates: [
            { id: 'github', description: 'GitHub', evidence: 'variable_name' },
          ],
        }
      } else if (path === '/projects/1/http-checks/1/run') {
        checks[0] = {
          ...checks[0],
          result: {
            status: 'error',
            checked_at: '2026-09-21T00:00:00Z',
            findings: [
              {
                code: 'authentication_rejected',
                status: 'error',
                message: 'The endpoint rejected the credential.',
              },
            ],
          },
        }
        body = checks[0]
      } else if (path === '/projects/1/http-checks/1') {
        if (route.request().method() === 'DELETE') checks = []
        else
          checks[0] = {
            ...checks[0],
            enabled: route.request().postDataJSON().enabled,
          }
        body = checks[0] ?? {}
      } else if (path === '/user/me')
        body = {
          id: 1,
          name: 'Owner',
          username: 'owner',
          email: 'owner@example.com',
          role: 'admin',
          mfa_enabled: false,
        }
      else if (
        path === '/projects/by-slug/example-app' ||
        path === '/projects/1'
      )
        body = project
      else if (path === '/projects/1/environments') body = [environment]
      else if (path === '/projects/1/env-vars')
        body = [
          {
            id: 1,
            key: 'APP_URL',
            value: '***',
            is_secret: false,
            environments: [environment],
            include_in_preview: true,
            created_at: 1,
            updated_at: 1,
          },
          {
            id: 2,
            key: 'PROVIDER_API_KEY',
            value: null,
            is_secret: true,
            environments: [environment],
            include_in_preview: false,
            created_at: 1,
            updated_at: 1,
          },
        ]
      else if (path === '/projects/1/env-vars/resolved')
        body = [
          {
            key: 'DATABASE_URL',
            value_preview: '***',
            environments: [environment],
            include_in_preview: false,
            source: {
              type: 'integration',
              service: {
                service_id: 4,
                service_name: 'primary-db',
                service_type: 'postgres',
                service_updated_at: '2026-09-01T00:00:00Z',
              },
            },
          },
        ]
      else if (path.endsWith('/env-example'))
        body = {
          path: '.env.example',
          variables: [
            {
              key: 'WEBHOOK_TOKEN',
              description: 'Authenticates incoming webhooks.',
              default_value: '',
            },
          ],
        }
      else if (path.endsWith('/compose-file')) body = { services: [] }
      else if (path.includes('/env-vars/') && path.endsWith('/value')) {
        reveals.push(path)
        body = { value: 'https://example.com' }
      } else if (path.endsWith('/last-deployment')) {
        await route.fulfill({ status: 404, json: { detail: 'No deployments' } })
        return
      } else if (path.includes('active-visitors')) body = { count: 0 }
      await route.fulfill({ json: body })
    })
    await page.goto('/projects/example-app/environment-variables')
    await expect(
      page.getByRole('button', { name: 'HTTP checks', exact: true })
    ).toHaveCount(0)
    const table = page.getByRole('table', { name: 'Environment variables' })
    await expect(table).toBeVisible()
    await expect(
      table.getByRole('columnheader', { name: 'Checks', exact: true })
    ).toBeVisible()
    const secret = table
      .getByRole('row')
      .filter({ hasText: 'PROVIDER_API_KEY' })
    await expect(secret.getByRole('button', { name: /Reveal/ })).toHaveCount(0)
    await expect(secret).toContainText('••••••••••••')
    expect(reveals).toEqual([])
    await secret.getByRole('button', { name: 'Checks: No issues' }).click()
    await expect(
      page.getByText('No checks configured.', { exact: false })
    ).toBeVisible()
    await page.keyboard.press('Escape')
    const missing = table.getByRole('row').filter({ hasText: 'WEBHOOK_TOKEN' })
    await missing.getByRole('button', { name: 'Checks: Value missing' }).click()
    await expect(
      page.getByText('Declared in your repository but not configured in Temps.')
    ).toBeVisible()
    await page.keyboard.press('Escape')
    await table
      .getByRole('row')
      .filter({ hasText: 'APP_URL' })
      .getByRole('button', { name: 'Reveal APP_URL' })
      .click()
    await expect(table).toContainText('https://example.com')
    expect(reveals).toHaveLength(1)
    await page.getByLabel('Select PROVIDER_API_KEY', { exact: true }).click()
    await expect(
      page.getByText('1 of 2 selected', { exact: true })
    ).toBeVisible()
    await secret.getByRole('button', { name: 'Edit', exact: true }).click()
    await expect(page.getByRole('dialog')).toContainText(
      'Stored secret values cannot be revealed.'
    )
    await page.keyboard.press('Escape')
    expect(reveals).toHaveLength(1)
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth > window.innerWidth
    )
    expect(overflow).toBe(false)
    await page.screenshot({
      path: testInfo.outputPath(`environment-variables-${width}.png`),
      fullPage: true,
    })
    const detailLink = secret.getByRole('link', {
      name: 'View PROVIDER_API_KEY details',
    })
    await expect(detailLink).toHaveAttribute(
      'href',
      '/projects/example-app/environment-variables/2'
    )
    await expect(detailLink).toHaveCSS('text-decoration-line', 'underline')
    await expect(detailLink.locator('svg')).toBeVisible()
    await secret.getByRole('cell').nth(2).click()
    await expect(page).toHaveURL(/environment-variables$/)
    await detailLink.focus()
    await page.keyboard.press('Enter')
    await expect(page).toHaveURL(/environment-variables\/2$/)
    await expect(page.getByRole('dialog')).toHaveCount(0)
    await page.reload()
    const details = page.getByRole('region', { name: 'Variable details' })
    await expect(details).not.toContainText('Value rotated')
    await details.getByRole('tab', { name: 'History', exact: true }).click()
    await expect(page).toHaveURL(/tab=history/)
    await page.reload()
    await expect(
      details.getByRole('tab', { name: 'History', exact: true })
    ).toHaveAttribute('aria-selected', 'true')
    await expect(details).toContainText('Value rotated')
    await expect(
      details.getByRole('table', { name: 'Variable activity' })
    ).toBeVisible()
    const historyPagination = details.getByRole('navigation', {
      name: 'History pagination',
    })
    await historyPagination
      .getByRole('button', {
        name: width < 640 ? 'Next page' : 'Go to next page',
        exact: true,
      })
      .click()
    await expect(page).toHaveURL(/page=2/)
    await expect(
      details.getByRole('table', { name: 'Variable activity' })
    ).toContainText('Variable created')
    await page.reload()
    await expect(
      details.getByRole('tab', { name: 'History', exact: true })
    ).toHaveAttribute('aria-selected', 'true')
    await expect(
      details.getByRole('table', { name: 'Variable activity' })
    ).toContainText('Variable created')
    await historyPagination
      .getByRole('button', {
        name: width < 640 ? 'Previous page' : 'Go to previous page',
        exact: true,
      })
      .click()
    await details.getByRole('tab', { name: /^Checks/ }).click()
    await expect(
      details.getByRole('table', { name: 'Variable activity' })
    ).toHaveCount(0)
    await expect(details).toContainText('Secret · write-only')
    const breadcrumbs = page.getByRole('navigation', {
      name: 'breadcrumb',
      exact: true,
    })
    await expect(
      breadcrumbs.getByRole('link', {
        name: 'Environment variables',
        exact: true,
      })
    ).toHaveAttribute('href', '/projects/example-app/environment-variables')
    await expect(breadcrumbs.locator('[aria-current="page"]')).toHaveText(
      'PROVIDER_API_KEY'
    )
    await expect(
      page.getByRole('navigation', { name: 'Variable navigation' })
    ).toHaveCount(0)

    expect(
      await details.evaluate((el) =>
        Math.abs(
          el.getBoundingClientRect().width -
            el.parentElement!.getBoundingClientRect().width
        )
      )
    ).toBeLessThan(2)

    await page.screenshot({
      path: testInfo.outputPath(`variable-details-${width}.png`),
      fullPage: true,
    })
    await details.getByRole('link', { name: 'Configure checks' }).click()
    await expect(page).toHaveURL(/environment-variables\/2\/checks$/)
    await page.reload()
    const dialog = page.getByRole('region', { name: 'Check configuration' })
    expect(
      await dialog.evaluate((el) =>
        Math.abs(
          el.getBoundingClientRect().width -
            el.parentElement!.getBoundingClientRect().width
        )
      )
    ).toBeLessThan(2)
    await expect(page.getByRole('dialog')).toHaveCount(0)
    await page.goBack()
    await expect(details).toBeVisible()
    await page.goForward()
    await expect(breadcrumbs.locator('[aria-current="page"]')).toHaveText(
      'Check configuration'
    )
    await expect(
      breadcrumbs.getByRole('link', { name: 'PROVIDER_API_KEY', exact: true })
    ).toHaveAttribute('href', '/projects/example-app/environment-variables/2')
    await breadcrumbs
      .getByRole('link', { name: 'PROVIDER_API_KEY', exact: true })
      .click()
    await expect(details).toBeVisible()
    await expect(breadcrumbs.locator('[aria-current="page"]')).toHaveText(
      'PROVIDER_API_KEY'
    )
    await details.getByRole('link', { name: 'Configure checks' }).click()
    await expect(dialog).toContainText('Checks for PROVIDER_API_KEY')
    await expect(
      dialog.getByRole('link', { name: 'configure notifications' })
    ).toBeVisible()
    const catalog = dialog.getByRole('region', {
      name: 'Credential provider catalog',
    })
    await expect(
      catalog.getByText('8 providers', { exact: false })
    ).toBeVisible()
    await catalog.getByRole('button', { name: 'Next providers' }).click()
    await expect(catalog.getByRole('img', { name: 'OpenAI' })).toBeVisible()
    const search = catalog.getByRole('textbox', {
      name: 'Search credential providers',
    })
    await search.fill('GITHUB user')
    await expect(catalog.getByRole('listitem')).toHaveCount(1)
    await expect(catalog.getByRole('img', { name: 'GitHub' })).toBeVisible()
    await catalog.getByRole('button', { name: 'Use GitHub template' }).click()
    await expect(dialog.getByLabel('Endpoint', { exact: true })).toHaveValue(
      'https://api.github.com/user'
    )
    expect(saved).toBeUndefined()
    await search.fill('expiration')
    await expect(catalog.getByRole('listitem')).toHaveCount(1)
    await expect(
      catalog.getByText('Configure manually', { exact: true })
    ).toBeVisible()
    await search.fill('no-provider-matches')
    await expect(
      catalog.getByText('No matching providers.', { exact: false })
    ).toBeVisible()
    await search.fill('')
    await expect(catalog.getByRole('listitem')).toHaveCount(6)
    await page.screenshot({
      path: testInfo.outputPath(`provider-catalog-${width}.png`),
      fullPage: true,
    })
    await catalog.getByRole('button', { name: 'Custom HTTP check' }).click()
    await dialog.getByRole('button', { name: 'Detect provider' }).click()
    await expect(dialog).toContainText('Suggested matches: github')
    await dialog
      .getByLabel('Endpoint', { exact: true })
      .fill('https://api.github.com/user')
    await dialog.getByRole('button', { name: 'Save check' }).click()
    await expect(dialog).toContainText('awaiting check')
    expect(saved?.env_var_id).toBe(2)
    expect(saved?.credential).toBeNull()
    expect(saved?.interval_seconds).toBe(86400)
    expect(reveals).toHaveLength(1)
    await dialog.getByRole('button', { name: 'Check now' }).click()
    await expect(
      dialog.getByRole('button', { name: /Checks:.*error/i })
    ).toBeVisible()
    await dialog.getByRole('button', { name: 'Pause', exact: true }).click()
    await expect(
      dialog.getByRole('button', { name: 'Resume', exact: true })
    ).toBeVisible()
    await dialog.getByRole('button', { name: 'Resume', exact: true }).click()
    await expect(
      dialog.getByRole('button', { name: 'Pause', exact: true })
    ).toBeVisible()
    await page.screenshot({
      path: testInfo.outputPath(`http-checks-${width}.png`),
      fullPage: true,
    })
    await dialog.getByRole('button', { name: 'Delete', exact: true }).click()
    await dialog.getByRole('button', { name: 'Confirm delete' }).click()
    await expect(dialog.getByRole('button', { name: 'Check now' })).toHaveCount(
      0
    )
    await page.keyboard.press('Escape')
    await page.route('**/api/projects/1/env-vars', async (route) => {
      await route.fulfill({
        json: [
          {
            id: 2,
            key: 'PROVIDER_API_KEY',
            value: null,
            is_secret: true,
            environments: [environment],
            include_in_preview: false,
            created_at: 1,
            updated_at: 1,
          },
        ],
      })
    })
    await page.route('**/api/projects/1/env-vars/resolved*', async (route) => {
      await route.fulfill({ json: [] })
    })
    await page.goto('/projects/example-app/environment-variables')
    await expect(
      page.getByRole('table', { name: 'Environment variables' })
    ).toBeVisible()
    await expect(
      page.getByRole('button', { name: /Show all|Reveal|Hide all/i })
    ).toHaveCount(0)
    expect(reveals).toHaveLength(1)
    await page.goto('/projects/example-app/environment-variables/999999')
    await expect(
      page.getByRole('heading', { name: 'Variable not found' })
    ).toBeVisible()
    await page
      .getByRole('link', { name: 'Back to environment variables', exact: true })
      .click()
    await expect(table).toBeVisible()
    await expect(breadcrumbs.locator('[aria-current="page"]')).toHaveText(
      'Environment variables'
    )
    await page.goto('/projects/example-app/environment-variables/not-a-number')
    await expect(
      page.getByRole('heading', { name: 'Variable not found' })
    ).toBeVisible()
    expect(errors).toEqual([])
  })
}
