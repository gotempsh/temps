// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import http from 'node:http'
import { expect, expectAppMounted, test, uniqueSlug } from '../fixtures'

/**
 * The console half of the first-run suite (.github/workflows/first-run.yml):
 * a new operator creates a Flexible project from a public Docker image in the
 * UI, and the app has to answer through the proxy.
 *
 * It deploys a real container, so it only runs when FIRST_RUN_UI=1 (set by
 * scripts/first-run/run-first-run.sh); the regular console suite skips it.
 * FIRST_RUN_PROXY_URL is the proxy origin the app is reached through.
 */

const IMAGE = process.env.FIRST_RUN_IMAGE ?? 'traefik/whoami:v1.10'
const IMAGE_PORT = process.env.FIRST_RUN_IMAGE_PORT ?? '80'
const PROXY_URL = process.env.FIRST_RUN_PROXY_URL ?? 'http://127.0.0.1:8760'

interface Project {
  id: number
  slug: string
}

interface Environment {
  id: number
  main_url: string
  is_preview?: boolean
}

interface Deployment {
  id: number
  status: string
}

/** GET through the proxy with the app's Host header (fetch cannot set Host). */
function getViaProxy(
  appUrl: string
): Promise<{ status: number; body: string }> {
  const proxy = new URL(PROXY_URL)
  const app = new URL(appUrl)
  return new Promise((resolve, reject) => {
    const request = http.request(
      {
        host: proxy.hostname,
        port: proxy.port || 80,
        path: '/',
        method: 'GET',
        headers: { Host: app.host },
        timeout: 10_000,
      },
      (response) => {
        let body = ''
        response.setEncoding('utf8')
        response.on('data', (chunk) => (body += chunk))
        response.on('end', () =>
          resolve({ status: response.statusCode ?? 0, body })
        )
      }
    )
    request.on('timeout', () => request.destroy(new Error('timed out')))
    request.on('error', reject)
    request.end()
  })
}

test.describe('first run: Flexible project from a Docker image', () => {
  test.skip(
    process.env.FIRST_RUN_UI !== '1',
    'deploys a real container; run through scripts/first-run/run-first-run.sh'
  )
  // Image pull + container start + route propagation on a cold runner.
  test.setTimeout(420_000)

  test('creates the project in the console and serves it through the proxy', async ({
    page,
    consoleErrors,
  }, testInfo) => {
    const name = uniqueSlug('first-run-ui', testInfo)
    const started = Date.now()

    await page.goto('/projects/new?source=manual')
    await expectAppMounted(page)
    await page.getByRole('tab', { name: 'Docker Image', exact: true }).click()

    // Flexible is the recommended default; select it explicitly so the spec
    // fails loudly if the default ever changes.
    await page.getByRole('heading', { name: 'Flexible', exact: true }).click()
    await page.getByLabel('Project Name').fill(name)
    await page.getByLabel('Docker Image (Optional)').fill(IMAGE)
    await page.getByLabel('Application Port').fill(IMAGE_PORT)
    await page
      .getByRole('button', { name: 'Create Project', exact: true })
      .click()

    try {
      // Creating with an image starts the first deployment and lands on the
      // project's deployments page.
      await page.waitForURL(new RegExp(`/projects/${name}/deployments`), {
        timeout: 60_000,
      })
      await expectAppMounted(page)

      const project = (await (
        await page.request.get(`/api/projects/by-slug/${name}`)
      ).json()) as Project
      const environments = (await (
        await page.request.get(`/api/projects/${project.id}/environments`)
      ).json()) as Environment[]
      const production =
        environments.find((env) => env.is_preview === false) ?? environments[0]
      expect(production, 'the project has an environment').toBeTruthy()

      let last: Deployment | undefined
      await expect
        .poll(
          async () => {
            const res = await page.request.get(
              `/api/projects/${project.id}/last-deployment`
            )
            last = res.ok() ? ((await res.json()) as Deployment) : undefined
            return last?.status ?? 'none'
          },
          {
            message: 'the first deployment should complete',
            timeout: 300_000,
            intervals: [3_000],
          }
        )
        .toMatch(/^(completed|failed|cancelled)$/)
      expect(last?.status, `deployment ${last?.id} status`).toBe('completed')

      await expect
        .poll(
          async () => {
            try {
              const res = await getViaProxy(production.main_url)
              return res.status === 200 && /Hostname:/.test(res.body)
            } catch {
              return false
            }
          },
          {
            message: `${production.main_url} should answer through the proxy`,
            timeout: 120_000,
            intervals: [2_000],
          }
        )
        .toBe(true)

      testInfo.annotations.push({
        type: 'time-to-success',
        description: `${((Date.now() - started) / 1000).toFixed(1)}s`,
      })
      expect(consoleErrors).toEqual([])
    } finally {
      const found = await page.request.get(`/api/projects/by-slug/${name}`)
      if (found.ok()) {
        const project = (await found.json()) as Project
        // Stop the container and drop its route before the project row goes.
        const deployment = await page.request.get(
          `/api/projects/${project.id}/last-deployment`
        )
        if (deployment.ok()) {
          const { id } = (await deployment.json()) as Deployment
          await page.request.delete(
            `/api/projects/${project.id}/deployments/${id}/teardown`
          )
        }
        await page.request.delete(`/api/projects/${project.id}`)
      }
    }
  })
})
