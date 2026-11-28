// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { jsonResponse, mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import {
  PluginImageError,
  addPluginFromImage,
  imagePinnedByDigest,
  imageWithContent,
  pluginImageCommand,
  pluginServiceName,
  shortImageId,
  type PluginImageProgress,
  type PluginImageStep,
  type PluginImageWait,
} from './plugin-image'

afterEach(restoreAllMockApis)

const IMAGE = 'registry.example.com/acme/reports:1.2.0'
const DIGEST = `registry.example.com/acme/reports@sha256:${'a'.repeat(64)}`
const VALUES = { image: IMAGE, name: 'reports', port: 8080 }

test('an image names its service, and only a digest names exact content', () => {
  expect(pluginServiceName(IMAGE)).toBe('reports')
  expect(pluginServiceName(DIGEST)).toBe('reports')
  expect(pluginServiceName(' localhost:5000/Audit_Export ')).toBe(
    'audit-export'
  )
  expect(pluginServiceName('registry.example.com/___:1')).toBe('')
  expect(pluginServiceName('')).toBe('')
  expect(imagePinnedByDigest(DIGEST)).toBe(true)
  expect(imagePinnedByDigest(IMAGE)).toBe(false)
  expect(imagePinnedByDigest('reports@sha256:abc')).toBe(false)
})

test('the terminal command names the image once one is given', () => {
  expect(pluginImageCommand()).toBe('temps plugin add --image <image>')
  expect(pluginImageCommand('  ')).toBe('temps plugin add --image <image>')
  expect(pluginImageCommand(` ${IMAGE} `)).toBe(
    `temps plugin add --image ${IMAGE}`
  )
})

test('an image is shown with the content it resolved to', () => {
  const id = `sha256:${'0123456789abcdef'.repeat(4)}`
  expect(shortImageId(id)).toBe('0123456789ab')
  expect(imageWithContent(IMAGE, id)).toBe(
    `the image ${IMAGE} (content 0123456789ab)`
  )
  expect(imageWithContent(IMAGE, null)).toBe(`the image ${IMAGE}`)
  // Uploaded from a client: no reference, only what it is.
  expect(imageWithContent(undefined, id)).toBe(
    'an uploaded image (content 0123456789ab)'
  )
  expect(imageWithContent(null, undefined)).toBe('')
})

/** No waiting between looks at a deployment, and a clock that can run out. */
const wait = (timeoutMs = 60_000): PluginImageWait => ({
  pollMs: 0,
  timeoutMs,
  sleep: async () => {},
})

interface Fake {
  /** A service of the chosen name already exists. */
  taken?: boolean
  /** The status each look at a deployment answers; the last one repeats. */
  statuses?: string[]
  /** Refusals of the add, one per attempt, before it is accepted. */
  refuseAdd?: Response[]
}

function temps(fake: Fake) {
  const bodies: Record<string, unknown> = {}
  let looks = 0
  let deployment = 41
  const refusals = [...(fake.refuseAdd ?? [])]
  const api = mockApi({
    'GET /projects/by-slug/reports': () =>
      fake.taken
        ? jsonResponse({ id: 3, name: 'reports', slug: 'reports' })
        : jsonResponse({ title: 'Not found' }, 404),
    'POST /projects': async (request) => {
      bodies.project = await request.json()
      return jsonResponse({ id: 7, name: 'reports', slug: 'reports' }, 201)
    },
    'GET /projects/7/environments': () =>
      jsonResponse([
        { id: 10, name: 'staging' },
        { id: 9, name: 'production' },
      ]),
    'POST /projects/7/environments/9/deploy/image': async (request) => {
      bodies.deploy = await request.json()
      deployment += 1
      looks = 0
      return jsonResponse(
        { id: deployment, slug: `deploy-${deployment}`, project_id: 7 },
        202
      )
    },
    'GET /projects/7/deployments/42': () => look(42),
    'GET /projects/7/deployments/43': () => look(43),
    'POST /x/plugins/services': async (request) => {
      bodies.add = await request.json()
      return (
        refusals.shift() ??
        jsonResponse(
          {
            name: 'reports',
            version: '1.2.0',
            project_id: 7,
            environment_id: 9,
          },
          201
        )
      )
    },
  })
  function look(id: number) {
    const statuses = fake.statuses ?? ['completed']
    const status = statuses[Math.min(looks++, statuses.length - 1)]
    return jsonResponse({ id, status, cancelled_reason: null })
  }
  const posts = () =>
    api.calls.filter(({ method }) => method === 'POST').map(({ path }) => path)
  return { api, bodies, posts }
}

function run(progress: PluginImageProgress, values = VALUES, waiting = wait()) {
  const steps: PluginImageStep[] = []
  const result = addPluginFromImage(
    values,
    progress,
    (step) => {
      if (steps[steps.length - 1] !== step) steps.push(step)
    },
    waiting
  )
  return { steps, result }
}

test('an image becomes a service, is deployed, waited for and added, in that order', async () => {
  const server = temps({ statuses: ['pending', 'running', 'completed'] })
  const { steps, result } = run({})
  const { added, projectSlug, deploymentId } = await result

  expect(added.name).toBe('reports')
  expect(projectSlug).toBe('reports')
  expect(deploymentId).toBe(42)
  expect(steps).toEqual(['service', 'deploy', 'live', 'add'])
  expect(server.posts()).toEqual([
    '/projects',
    '/projects/7/environments/9/deploy/image',
    '/x/plugins/services',
  ])
  expect(server.bodies.project).toMatchObject({
    name: 'reports',
    source_type: 'docker_image',
    project_type: 'docker',
    exposed_port: 8080,
    automatic_deploy: false,
  })
  // A plugin answers its health check on /health, not on "/".
  expect(server.bodies.deploy).toEqual({
    image_ref: IMAGE,
    health_check_path: '/health',
  })
  // The production environment, wherever it is listed.
  // The deployment that was waited for is the one that is added.
  expect(server.bodies.add).toEqual({
    project_id: 7,
    environment_id: 9,
    deployment_id: 42,
  })
})

test('an image is never deployed over a service that only shares its name', async () => {
  const server = temps({ taken: true })
  const failure = await run({}).result.catch((error: unknown) => error)
  expect(failure).toBeInstanceOf(PluginImageError)
  expect((failure as PluginImageError).title).toBe(
    'A service with this name exists'
  )
  expect((failure as PluginImageError).detail).toContain('From a service')
  expect(server.posts()).toEqual([])
})

test('a deployment that fails adds nothing, and trying again deploys to the same service', async () => {
  const server = temps({ statuses: ['running', 'failed'] })
  const progress: PluginImageProgress = {}
  const failure = await run(progress).result.catch((error: unknown) => error)
  expect((failure as PluginImageError).title).toBe(
    'The deployment did not become live'
  )
  expect((failure as PluginImageError).detail).toContain(
    'Deployment 42 of reports ended as failed'
  )
  expect(server.posts()).not.toContain('/x/plugins/services')
  // What was made is kept for the next try.
  expect(progress).toMatchObject({ projectId: 7, projectSlug: 'reports' })
  expect(progress.deploymentId).toBeUndefined()

  // The second deployment starts over; it fails the same way here, which is
  // enough to see what a retry sends.
  await run(progress).result.catch(() => {})
  expect(server.posts()).toEqual([
    '/projects',
    '/projects/7/environments/9/deploy/image',
    '/projects/7/environments/9/deploy/image',
  ])
})

test('a refused add is tried again without creating or deploying anything twice', async () => {
  const server = temps({
    refuseAdd: [
      jsonResponse(
        { title: 'Verification required', code: 'STEP_UP_REQUIRED' },
        403
      ),
    ],
  })
  const progress: PluginImageProgress = {}
  const refused = await run(progress).result.catch((error: unknown) => error)
  // The server's own problem is passed on untouched, so a verification
  // request is recognised.
  expect(refused).toMatchObject({ code: 'STEP_UP_REQUIRED' })
  expect(progress).toMatchObject({ deploymentId: 42, live: true })

  const again = run(progress)
  const { added } = await again.result
  expect(added.name).toBe('reports')
  expect(again.steps).toEqual(['add'])
  expect(server.posts()).toEqual([
    '/projects',
    '/projects/7/environments/9/deploy/image',
    '/x/plugins/services',
    '/x/plugins/services',
  ])
})

test('a deployment that is still not live is not given up on: trying again keeps waiting for it', async () => {
  const server = temps({ statuses: ['running'] })
  const progress: PluginImageProgress = {}
  const failure = await run(progress, VALUES, wait(0)).result.catch(
    (error: unknown) => error
  )
  expect((failure as PluginImageError).title).toBe(
    'The deployment is not live yet'
  )
  expect(progress.deploymentId).toBe(42)
  expect(server.posts()).toEqual([
    '/projects',
    '/projects/7/environments/9/deploy/image',
  ])
})

test('another image is deployed to the service that was made; another name starts over', async () => {
  const server = temps({})
  const progress: PluginImageProgress = {}
  await run(progress).result
  expect(progress.deploymentId).toBe(42)

  await run(progress, { ...VALUES, image: DIGEST }).result
  expect(progress.deploymentId).toBe(43)
  expect(server.posts().filter((path) => path === '/projects')).toHaveLength(1)
  expect(server.bodies.deploy).toMatchObject({ image_ref: DIGEST })

  const renamed: PluginImageProgress = { ...progress }
  await run(renamed, { ...VALUES, name: 'audit' }).result.catch(() => {})
  // Nothing made for the other name is reused: the service is looked up by
  // its new name first.
  expect(server.api.called('GET', '/projects/by-slug/audit')).toBe(true)
  expect(renamed.name).toBe('audit')
})
