// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { execFile } from 'node:child_process'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { promisify } from 'node:util'
import {
  createProject,
  createEnvironment,
  deleteProject,
  deployFromImage,
  getContainerDetail,
  getDeployment,
  getEnvironment,
  getEnvironments,
  getProject,
  listContainerHistory,
  listContainers,
  promoteDeployment,
  rollbackToDeployment,
  teardownDeployment,
  updateProjectDeploymentConfig,
  type HealthCheckConfig,
} from '../../src/api/client'
import { createClient } from '../../src/api/client/client'
import { expect, test, uniqueSlug } from '../fixtures'

const docker = promisify(execFile)
const registry = process.env.HEALTH_PROBE_E2E_REGISTRY
const proxyURL =
  process.env.HEALTH_PROBE_E2E_PROXY_URL ?? 'http://127.0.0.1:8080'

// Each target has a distinct image configuration; the worker has no EXPOSE.
const dockerfile = `# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
FROM busybox:1.37.0 AS base
RUN mkdir /www && echo temps-health-web > /www/index.html
USER 65534:65534
FROM base AS web
EXPOSE 8080
CMD ["httpd", "-f", "-p", "8080", "-h", "/www"]
FROM base AS worker
CMD ["sleep", "86400"]
FROM base AS crash
CMD ["sh", "-c", "sleep 3; exit 1"]
`

test.describe('live deployment health probes', () => {
  test.skip(
    !registry,
    'requires a disposable instance and HEALTH_PROBE_E2E_REGISTRY'
  )
  test.setTimeout(900_000)

  test('gates each probe, preserves the current deployment on failure and withdraws web routes', async ({
    page,
    baseURL,
    consoleErrors,
  }, testInfo) => {
    // Use the generated bindings with Playwright's authenticated cookie jar.
    // No mocked API, synthetic database rows or extra bearer credential.
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
          const body = [204, 205, 304].includes(response.status())
            ? null
            : new Uint8Array(await response.body())
          return new Response(body, {
            status: response.status(),
            headers: response.headers(),
          })
        },
        { preconnect: fetch.preconnect }
      ),
    })
    const api = { client, throwOnError: true as const }
    const name = uniqueSlug('e2e-health', testInfo)
    const scratch = await mkdtemp(join(tmpdir(), 'temps-health-e2e-'))
    const images: string[] = []
    const deployments: number[] = []
    let projectId: number | undefined

    try {
      await writeFile(join(scratch, 'Dockerfile'), dockerfile)
      const image = {} as Record<'web' | 'worker' | 'crash', string>
      for (const target of ['web', 'worker', 'crash'] as const) {
        const tag = `${registry}/${name}-${target}:test`
        images.push(tag)
        await docker(
          'docker',
          ['build', '--target', target, '-t', tag, scratch],
          { timeout: 180_000 }
        )
        await docker('docker', ['push', tag], { timeout: 120_000 })
        image[target] = tag
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
      const env = environments.find((value) => !value.is_preview)
      if (!env)
        throw new Error(`Project ${project.id} has no production environment`)
      const envPath = { ...path, environment_id: env.id }

      const configure = async (
        health: HealthCheckConfig,
        port: number | null
      ) => {
        await updateProjectDeploymentConfig({
          ...api,
          path,
          body: { health, exposedPort: port, healthCheckTimeoutSeconds: 30 },
        })
        // The project PATCH leaves health unchanged for null. An empty
        // object replaces a saved probe with automatic selection/defaults.
        const { data: saved } = await getProject({
          ...api,
          path: { id: project.id },
        })
        expect(saved.deployment_config?.health?.type ?? null).toBe(
          health.type ?? null
        )
        expect(saved.deployment_config?.health?.command).toEqual(
          health.command ?? []
        )
        expect(saved.deployment_config?.exposedPort ?? null).toBe(port)
      }
      const deploy = async (ref: string, expected: 'completed' | 'failed') => {
        const { data: candidate } = await deployFromImage({
          ...api,
          path: envPath,
          body: { image_ref: ref },
        })
        deployments.push(candidate.id)
        await expect
          .poll(
            async () => {
              const { data } = await getDeployment({
                ...api,
                path: { ...path, deployment_id: candidate.id },
              })
              return data.status
            },
            { timeout: 120_000, intervals: [1000] }
          )
          .toMatch(/^(completed|failed|cancelled)$/)
        const { data } = await getDeployment({
          ...api,
          path: { ...path, deployment_id: candidate.id },
        })
        expect(
          data.status,
          `deployment ${candidate.id}: ${JSON.stringify(data.failure)}`
        ).toBe(expected)
        return data
      }
      const currentIs = async (id: number) => {
        const { data } = await getEnvironment({
          ...api,
          path: { ...path, env_id: env.id },
        })
        expect(data.current_deployment_id).toBe(id)
      }
      const webBody = async () => {
        const response = await page.request.get(proxyURL, {
          headers: { Host: new URL(env.main_url).host },
          maxRedirects: 0,
        })
        return (await response.text()).trim()
      }

      await test.step('probe settings survive saving after invalid hidden fields', async () => {
        await page.goto(
          `/projects/${project.slug}/settings/delivery#deployment`
        )
        const probe = page.getByRole('combobox', {
          name: 'Health probe',
          exact: true,
        })
        const choose = async (option: string) => {
          await probe.click()
          await page.getByRole('option', { name: option, exact: true }).click()
        }
        await choose('TCP')
        await page
          .getByLabel('Probe timeout (seconds)', { exact: true })
          .fill('')
        await choose('HTTP')
        const save = async () => {
          const [response] = await Promise.all([
            page.waitForResponse(
              (response) =>
                response.request().method() === 'PATCH' &&
                response
                  .url()
                  .endsWith(`/projects/${project.id}/deployment-config`)
            ),
            page
              .getByRole('button', { name: 'Save Configuration', exact: true })
              .click(),
          ])
          expect(response.ok()).toBe(true)
        }
        await save()
        await expect(
          page.getByText('Deployment configuration updated successfully', {
            exact: true,
          })
        ).toBeVisible()
        await page.getByLabel('HTTP health path', { exact: true }).fill('')
        await choose('Process (background worker)')
        await save()
        await expect(
          page.getByText('Deployment configuration updated successfully', {
            exact: true,
          })
        ).toBeVisible()
        await page.reload()
        await expect(probe).toHaveText('Process (background worker)')
      })

      await test.step('default HTTP and explicit TCP serve real proxy traffic', async () => {
        await configure({}, 8080)
        await deploy(image.web, 'completed')
        await expect.poll(webBody).toBe('temps-health-web')
        await configure({ type: 'tcp', interval: 1, timeout: 1 }, 8080)
        await deploy(image.web, 'completed')
        await expect.poll(webBody).toBe('temps-health-web')
      })

      const { data: baseline } = await getEnvironment({
        ...api,
        path: { ...path, env_id: env.id },
      })
      const webDeployment = baseline.current_deployment_id
      if (!webDeployment) throw new Error('Missing current web deployment')
      const failures: [string, HealthCheckConfig, number | null, string][] = [
        ['HTTP closed port', { type: 'http' }, 8099, image.web],
        [
          'TCP closed port',
          { type: 'tcp', interval: 1, timeout: 1 },
          8099,
          image.web,
        ],
        [
          'process crash after startup',
          { type: 'process', stable_for: 15 },
          null,
          image.crash,
        ],
        [
          'exec nonzero',
          { type: 'exec', command: ['sh', '-c', 'exit 1'], interval: 1 },
          null,
          image.worker,
        ],
        [
          'exec timeout',
          { type: 'exec', command: ['sleep', '60'], timeout: 1 },
          null,
          image.worker,
        ],
        [
          'exec missing executable',
          { type: 'exec', command: ['missing-health-command'] },
          null,
          image.worker,
        ],
      ]
      for (const [label, health, port, ref] of failures) {
        await test.step(`${label} leaves the working web deployment current`, async () => {
          await configure(health, port)
          const failed = await deploy(ref, 'failed')
          await currentIs(webDeployment)
          await expect.poll(webBody).toBe('temps-health-web')
          if (label === 'exec timeout') {
            const { data: history } = await listContainerHistory({
              ...api,
              path: envPath,
            })
            const retained = history.containers.find(
              (row) => row.deployment_id === failed.id
            )
            if (!retained)
              throw new Error(
                `Missing timed-out candidate for deployment ${failed.id}`
              )
            // Include the old cleanup retry window: a retained failure must
            // remain stopped after an asynchronous cleanup retry could run.
            await new Promise((resolve) => setTimeout(resolve, 6000))
            const { stdout } = await docker('docker', [
              'inspect',
              '--format',
              '{{.State.Running}}',
              retained.container_id,
            ])
            expect(stdout.trim()).toBe('false')
          }
        })
      }
      await test.step('unknown probe is rejected before persistence', async () => {
        const response = await updateProjectDeploymentConfig({
          client,
          path,
          body: { health: { type: 'unsupported-probe' } },
        })
        expect(response.response?.status).toBe(400)
        await currentIs(webDeployment)
      })

      await test.step('portless default process records health and withdraws the old route', async () => {
        await configure({}, null)
        const worker = await deploy(image.worker, 'completed')
        await currentIs(worker.id)
        expect(worker.screenshot_location).toBeNull()
        const { data } = await listContainers({ ...api, path: envPath })
        expect(data.containers).toHaveLength(1)
        expect(data.containers[0].container_port).toBeNull()
        expect(data.containers[0].health_state).toBe('healthy')
        expect(data.containers[0].health_checked_at).toBeTruthy()
        expect(data.containers[0].service_urls ?? []).toEqual([])
        // A positive web response above makes this a real transition check.
        await expect.poll(webBody).not.toBe('temps-health-web')
        await page.goto(
          `/projects/${project.slug}/environments?environment=${env.id}`
        )
        await expect(
          page.getByText('Background worker', { exact: true }).first()
        ).toBeVisible()
      })

      await test.step('exec succeeds, while a failed exec keeps output out of persisted details', async () => {
        await configure(
          { type: 'exec', command: ['sh', '-c', 'exit 0'], interval: 1 },
          null
        )
        const good = await deploy(image.worker, 'completed')
        await configure(
          {
            type: 'exec',
            command: [
              'sh',
              '-c',
              "printf 'private-probe-marker'; head -c 8192 /dev/zero; exit 127",
            ],
          },
          null
        )
        const bad = await deploy(image.worker, 'failed')
        await currentIs(good.id)
        const { data: history } = await listContainerHistory({
          ...api,
          path: envPath,
        })
        const retained = history.containers.find(
          (row) => row.deployment_id === bad.id
        )
        expect(retained, 'failed exec candidate must be retained').toBeTruthy()
        if (!retained)
          throw new Error(`Missing retained container for deployment ${bad.id}`)
        const { data: detail } = await getContainerDetail({
          ...api,
          path: { ...envPath, container_id: retained.container_id },
        })
        expect(detail.health_state).toBe('unhealthy')
        expect(detail.health_detail).toContain('exit 127')
        expect(detail.health_detail).not.toContain('private-probe-marker')
        expect(detail.status).toBe('retained:stopped-after-failed-readiness')
        const { stdout } = await docker('docker', [
          'inspect',
          '--format',
          '{{.State.Running}}',
          retained.container_id,
        ])
        expect(stdout.trim()).toBe('false')
      })

      await test.step('rollback preserves the configured exec gate', async () => {
        const { data: before } = await getEnvironment({
          ...api,
          path: { ...path, env_id: env.id },
        })
        const { data: rollback } = await rollbackToDeployment({
          ...api,
          path: { ...path, deployment_id: webDeployment },
        })
        deployments.push(rollback.id)
        await expect
          .poll(
            async () => {
              const { data } = await getDeployment({
                ...api,
                path: { ...path, deployment_id: rollback.id },
              })
              return data.status
            },
            { timeout: 120_000, intervals: [1000] }
          )
          .toBe('failed')
        if (!before.current_deployment_id)
          throw new Error('Missing current worker deployment')
        await currentIs(before.current_deployment_id)
      })
      await test.step('promotion preserves the configured exec gate in a new environment', async () => {
        const { data: staging } = await createEnvironment({
          ...api,
          path,
          body: { name: 'staging', branch: 'staging' },
        })
        const { data: promoted } = await promoteDeployment({
          ...api,
          path: { ...path, deployment_id: webDeployment },
          body: { target_environment_id: staging.id },
        })
        deployments.push(promoted.id)
        await expect
          .poll(
            async () => {
              const { data } = await getDeployment({
                ...api,
                path: { ...path, deployment_id: promoted.id },
              })
              return data.status
            },
            { timeout: 120_000, intervals: [1000] }
          )
          .toBe('failed')
        const { data } = await getEnvironment({
          ...api,
          path: { ...path, env_id: staging.id },
        })
        expect(data.current_deployment_id).toBeNull()
      })
      expect(consoleErrors).toEqual([])
    } finally {
      // Cleanup is limited to IDs/tags created by this test, including failures.
      const errors: string[] = []
      const ownedContainers = new Set<string>()
      if (projectId !== undefined) {
        const environments = await getEnvironments({
          client,
          path: { project_id: projectId },
        })
        if (!environments.data)
          errors.push('could not list owned environments for cleanup')
        for (const env of environments.data ?? []) {
          const history = await listContainerHistory({
            client,
            path: { project_id: projectId, environment_id: env.id },
          })
          if (!history.data)
            errors.push(
              `could not list owned containers for environment ${env.id}`
            )
          for (const row of history.data?.containers ?? []) {
            if (
              deployments.includes(row.deployment_id) &&
              /^[a-f0-9]{64}$/.test(row.container_id)
            )
              ownedContainers.add(row.container_id)
          }
        }
        for (const deployment_id of deployments.reverse()) {
          const response = await teardownDeployment({
            client,
            path: { project_id: projectId, deployment_id },
          })
          if (!response.response?.ok && response.response?.status !== 404)
            errors.push(
              `teardown ${deployment_id}: HTTP ${response.response?.status ?? 'network error'}`
            )
        }
        const response = await deleteProject({
          client,
          path: { id: projectId },
        })
        if (!response.response?.ok)
          errors.push(
            `delete project: HTTP ${response.response?.status ?? 'network error'}`
          )
      }
      for (const id of ownedContainers) {
        await docker('docker', ['container', 'rm', '--force', id]).catch(
          (error: unknown) => {
            if (!/No such container/i.test(String(error)))
              errors.push(`remove owned container ${id}`)
          }
        )
      }
      for (const tag of images)
        await docker('docker', ['image', 'rm', tag]).catch(() => undefined)
      await rm(scratch, { recursive: true, force: true })
      expect(errors, 'owned deployment cleanup').toEqual([])
    }
  })
})
