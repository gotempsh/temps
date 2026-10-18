// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { execFile } from 'node:child_process'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { promisify } from 'node:util'
import {
  createProject,
  deleteProject,
  deployFromImage,
  getDeployment,
  getEnvironments,
  getRuntimeHealth,
  listContainers,
  listProjectAlarms,
  teardownDeployment,
  updateProjectDeploymentConfig,
  type HealthCheckConfig,
} from '../../src/api/client'
import { createClient } from '../../src/api/client/client'
import { expect, test, uniqueSlug } from '../fixtures'

const run = promisify(execFile)
const registry = process.env.HEALTH_PROBE_E2E_REGISTRY
const dockerfile = `FROM busybox:1.37.0 AS base
RUN mkdir /www && chmod 777 /www && echo healthy > /www/health
USER 65534:65534
FROM base AS web
EXPOSE 8080
CMD ["httpd", "-f", "-p", "8080", "-h", "/www"]
FROM base AS worker
CMD ["sleep", "86400"]
`

test.describe('continuous runtime health', () => {
  test.skip(
    !registry,
    'requires a disposable instance and HEALTH_PROBE_E2E_REGISTRY'
  )
  test.setTimeout(600_000)

  test('measures runtime failures, resolves one alarm and shows worker coverage honestly', async ({
    page,
    baseURL,
    consoleErrors,
  }, testInfo) => {
    const client = createClient({
      baseUrl: new URL('/api', baseURL).href,
      fetch: Object.assign(
        async (
          input: Parameters<typeof fetch>[0],
          init?: Parameters<typeof fetch>[1]
        ) => {
          const request =
            input instanceof Request ? input : new Request(input, init)
          const response = await page.request.fetch(request.url, {
            method: request.method,
            headers: Object.fromEntries(request.headers),
            data: request.body ? await request.text() : undefined,
            timeout: 60_000,
          })
          return new Response(
            [204, 205, 304].includes(response.status())
              ? null
              : new Uint8Array(await response.body()),
            {
              status: response.status(),
              headers: response.headers(),
            }
          )
        },
        { preconnect: fetch.preconnect }
      ),
    })
    const api = { client, throwOnError: true as const }
    const name = uniqueSlug('e2e-runtime-health', testInfo)
    const scratch = await mkdtemp(join(tmpdir(), 'temps-runtime-health-e2e-'))
    const images: string[] = []
    const deployments: number[] = []
    const containers = new Set<string>()
    let projectId: number | undefined
    try {
      await writeFile(join(scratch, 'Dockerfile'), dockerfile)
      for (const target of ['web', 'worker']) {
        const tag = `${registry}/${name}-${target}:test`
        images.push(tag)
        await run('docker', ['build', '--target', target, '-t', tag, scratch], {
          timeout: 180_000,
        })
        await run('docker', ['push', tag], { timeout: 120_000 })
      }
      const { data: project } = await createProject({
        ...api,
        body: {
          name,
          source_type: 'docker_image',
          preset: 'dockerfile',
          directory: '.',
          main_branch: 'main',
          storage_service_ids: [],
          automatic_deploy: false,
          exposed_port: 8080,
          performance_metrics_enabled: false,
        },
      })
      projectId = project.id
      const path = { project_id: project.id }
      const { data: environments } = await getEnvironments({ ...api, path })
      const environment = environments.find((value) => !value.is_preview)
      if (!environment) throw new Error('Missing production environment')
      const envPath = { ...path, environment_id: environment.id }
      const health = () =>
        getRuntimeHealth({ ...api, path: envPath }).then(
          (result) => result.data
        )
      const configure = (probe: HealthCheckConfig, port: number | null) =>
        updateProjectDeploymentConfig({
          ...api,
          path,
          body: {
            health: probe,
            exposedPort: port,
            healthCheckTimeoutSeconds: 30,
          },
        })
      const deploy = async (image: string) => {
        const { data: deployment } = await deployFromImage({
          ...api,
          path: envPath,
          body: { image_ref: image },
        })
        deployments.push(deployment.id)
        await expect
          .poll(
            async () =>
              (
                await getDeployment({
                  ...api,
                  path: { ...path, deployment_id: deployment.id },
                })
              ).data.status,
            { timeout: 120_000 }
          )
          .toMatch(/^(completed|failed|cancelled)$/)
        expect(
          (
            await getDeployment({
              ...api,
              path: { ...path, deployment_id: deployment.id },
            })
          ).data.status
        ).toBe('completed')
        const { data } = await listContainers({ ...api, path: envPath })
        for (const container of data.containers)
          containers.add(container.container_id)
        const container = data.containers[0]
        if (!container) throw new Error('Deployment has no recorded container')
        return container.container_id
      }
      const alarms = async () =>
        (
          await listProjectAlarms({
            ...api,
            path,
            query: { alarm_type: 'runtime_probe_failed', page_size: 100 },
          })
        ).data.items

      await configure(
        { type: 'http', path: '/health', interval: 2, timeout: 2, retries: 2 },
        8080
      )
      const web = await deploy(images[0])
      await expect
        .poll(async () => (await health()).state, { timeout: 90_000 })
        .toBe('healthy')
      await test.step('failed checks leave the application running and emit one debounced alarm', async () => {
        await run('docker', ['exec', web, 'rm', '/www/health'])
        await expect
          .poll(async () => (await health()).state, { timeout: 60_000 })
          .toBe('unhealthy')
        await expect
          .poll(
            async () =>
              (await alarms()).filter((value) => value.status === 'firing')
                .length,
            { timeout: 60_000 }
          )
          .toBe(1)
        const { stdout } = await run('docker', [
          'inspect',
          '--format',
          '{{.State.Running}} {{.RestartCount}}',
          web,
        ])
        expect(stdout.trim()).toBe('true 0')
        const current = await health()
        expect(current.coverage.unhealthy_seconds).toBeGreaterThan(0)
        expect(current.probe?.source).toBe('deployment_settings')
      })
      await test.step('two successes resolve the existing alarm without creating another', async () => {
        await run('docker', ['exec', web, 'touch', '/www/health'])
        await expect
          .poll(async () => (await health()).state, { timeout: 60_000 })
          .toBe('healthy')
        await expect
          .poll(async () => (await alarms()).map((value) => value.status), {
            timeout: 60_000,
          })
          .toEqual(['resolved'])
      })
      await test.step('portless workers get runtime coverage and their effective immutable configuration', async () => {
        await configure(
          {
            type: 'process',
            stable_for: 2,
            interval: 2,
            timeout: 2,
            retries: 2,
          },
          null
        )
        await deploy(images[1])
        await expect
          .poll(async () => (await health()).state, { timeout: 90_000 })
          .toBe('healthy')
        await configure({ type: 'exec', command: ['sh', '-c', 'exit 7'] }, null)
        expect((await health()).probe?.kind).toBe('process')
        expect((await health()).portless).toBe(true)
        await page.goto(`/projects/${project.slug}/monitors`)
        await expect(
          page.getByRole('heading', { name: 'production runtime health' })
        ).toBeVisible()
        await expect(
          page.getByText('Coverage · eligible time', { exact: true })
        ).toBeVisible()
        await expect(
          page.getByText('Unknown time', { exact: true })
        ).toBeVisible()
        await expect(
          page.getByText(/Source: deployment settings/)
        ).toBeVisible()
        await expect(page.getByText('WEB', { exact: true })).toHaveCount(0)
        await page
          .getByRole('button', { name: 'Pause monitoring', exact: true })
          .click()
        await expect.poll(async () => (await health()).reason).toBe('paused')
        expect((await health()).state).toBe('unknown')
        await page
          .getByRole('button', { name: 'Resume monitoring', exact: true })
          .click()
        await expect
          .poll(async () => (await health()).state, { timeout: 90_000 })
          .toBe('healthy')
        expect((await health()).coverage.excluded_seconds).toBeGreaterThan(0)
      })
      await test.step('runtime exec stays explicitly unavailable without using deployment cleanup', async () => {
        await configure(
          { type: 'exec', command: ['true'], interval: 2, timeout: 2 },
          null
        )
        const worker = await deploy(images[1])
        await expect
          .poll(async () => (await health()).reason)
          .toBe('runtime_exec_unavailable')
        expect((await health()).state).toBe('unknown')
        const serialized = JSON.stringify(await health())
        expect(serialized).not.toContain('command')
        const { stdout } = await run('docker', [
          'inspect',
          '--format',
          '{{.State.Running}} {{.RestartCount}}',
          worker,
        ])
        expect(stdout.trim()).toBe('true 0')
      })
      expect(consoleErrors).toEqual([])
    } finally {
      const errors: string[] = []
      if (projectId !== undefined) {
        for (const deployment_id of deployments.reverse()) {
          const result = await teardownDeployment({
            client,
            path: { project_id: projectId, deployment_id },
          })
          if (!result.response?.ok && result.response?.status !== 404)
            errors.push(`teardown ${deployment_id}`)
        }
        const result = await deleteProject({ client, path: { id: projectId } })
        if (!result.response?.ok) errors.push('delete owned project')
      }
      for (const container of containers)
        await run('docker', ['container', 'rm', '--force', container]).catch(
          (error: unknown) => {
            if (!/No such container/i.test(String(error)))
              errors.push('remove owned fixture container')
          }
        )
      for (const image of images)
        await run('docker', ['image', 'rm', image]).catch(() => undefined)
      await rm(scratch, { recursive: true, force: true })
      expect(errors, 'owned fixture cleanup').toEqual([])
    }
  })
})
