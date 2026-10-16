// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, expectAppMounted, test, uniqueSlug } from '../fixtures'

/**
 * A service created from the manual Docker-image flow with `?group=<id>` must
 * join the project before its first deployment is planned, so that deployment
 * already carries the project's shared variables.
 */

interface IdRow {
  id: number
}

interface Deployment {
  id: number
  status: string
}

interface ContainerList {
  containers: { container_id: string }[]
}

test.describe('project groups: first deployment carries shared variables', () => {
  test.setTimeout(240_000)

  test('manual image flow joins the project before the first deploy', async ({
    page,
  }, testInfo) => {
    const groupSlug = uniqueSlug('e2e-b-fd-grp', testInfo)
    const name = uniqueSlug('e2e-b-fd', testInfo)

    const group = (await (
      await page.request.post('/api/project-groups', {
        data: { name: groupSlug, slug: groupSlug },
      })
    ).json()) as IdRow
    const variable = await page.request.post(
      `/api/project-groups/${group.id}/env-vars`,
      { data: { key: 'SHARED_FROM_GROUP', value: 'first-deploy-ok' } }
    )
    expect(variable.status()).toBe(201)

    try {
      await page.goto(`/projects/new?source=manual&group=${group.id}`)
      await expectAppMounted(page)
      await page.getByRole('tab', { name: 'Docker Image', exact: true }).click()
      await page.getByRole('heading', { name: 'Flexible', exact: true }).click()
      await page.locator('input[name="name"]').fill(name)
      await page
        .locator('input[name="imageUrl"]')
        .fill('mendhak/http-https-echo:35')
      await page.locator('input[name="port"]').fill('8080')
      await page
        .getByRole('button', { name: 'Create service', exact: true })
        .click()

      // The group has a single member once the flow finishes.
      await expect
        .poll(
          async () => {
            const res = await page.request.get(`/api/project-groups/${group.id}`)
            const body = (await res.json()) as { projects: { slug: string }[] }
            return body.projects.map((p) => p.slug)
          },
          { timeout: 60_000 }
        )
        .toContain(name)

      const project = (await (
        await page.request.get(`/api/projects/by-slug/${name}`)
      ).json()) as IdRow
      const envs = (await (
        await page.request.get(`/api/projects/${project.id}/environments`)
      ).json()) as IdRow[]
      const envId = envs[0].id

      let last: Deployment | undefined
      await expect
        .poll(
          async () => {
            const res = await page.request.get(
              `/api/projects/${project.id}/last-deployment`
            )
            last = res.ok()
              ? ((await res.json()) as { deployment: Deployment | null })
                  .deployment ?? undefined
              : undefined
            return last?.status ?? 'none'
          },
          { timeout: 300_000, intervals: [3_000] }
        )
        .toMatch(/^(completed|failed|cancelled)$/)
      expect(last?.status).toBe('completed')

      // The deployment under test is the first one for this project.
      const deployments = (await (
        await page.request.get(`/api/projects/${project.id}/deployments`)
      ).json()) as { deployments?: IdRow[]; total?: number }
      expect(deployments.total ?? deployments.deployments?.length).toBe(1)

      const containers = (await (
        await page.request.get(
          `/api/projects/${project.id}/environments/${envId}/containers`
        )
      ).json()) as ContainerList
      const containerId = containers.containers[0].container_id
      const value = await page.request.get(
        `/api/projects/${project.id}/environments/${envId}/containers/${containerId}/environment/SHARED_FROM_GROUP`
      )
      expect(value.status()).toBe(200)
      expect(await value.json()).toEqual({ value: 'first-deploy-ok' })
    } finally {
      const found = await page.request.get(`/api/projects/by-slug/${name}`)
      if (found.ok()) {
        const project = (await found.json()) as IdRow
        const latest = await page.request.get(
          `/api/projects/${project.id}/last-deployment`
        )
        if (latest.ok()) {
          const d = ((await latest.json()) as { deployment: Deployment | null })
            .deployment
          if (d) {
            await page.request.delete(
              `/api/projects/${project.id}/deployments/${d.id}/teardown`
            )
          }
        }
        await page.request.delete(`/api/projects/${project.id}`)
      }
      await page.request.delete(`/api/project-groups/${group.id}`)
    }
  })
})
