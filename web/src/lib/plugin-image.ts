// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  addServicePlugin,
  createProject,
  deployFromImage,
  getDeployment,
  getEnvironments,
  getProjectBySlug,
} from '@/api/client/sdk.gen'
import type { AddedServicePluginResponse } from '@/api/client/types.gen'
import { firstDeployPhase } from './first-deploy'

/** The command that adds a plugin from this image, for a terminal. */
export function pluginImageCommand(image?: string): string {
  const named = image?.trim()
  return `temps plugin add --image ${named ? named : '<image>'}`
}

/** Whether an image reference names exact content, not a tag that can move. */
export function imagePinnedByDigest(image: string): boolean {
  return /@sha256:[0-9a-f]{64}$/.test(image.trim())
}

/**
 * A service name for an image: the last part of its repository, without the
 * tag or digest. "registry.example.com/acme/reports:1.2.0" is "reports".
 * Empty when the image names nothing a service can be called.
 */
export function pluginServiceName(image: string): string {
  const repository = image
    .trim()
    .replace(/@.*$/, '')
    .replace(/:[^/:]*$/, '')
  return (repository.split('/').pop() ?? '')
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, '-')
    .replace(/^-+|-+$/g, '')
}

/** The first twelve characters of an image's content, as Docker prints it. */
export function shortImageId(imageId: string): string {
  return imageId.replace(/^sha256:/, '').slice(0, 12)
}

/**
 * The image of a build, in words: its reference and the content that
 * reference resolved to when Temps recorded it ("the image x (content y)"),
 * or "an uploaded image (content y)" for one uploaded from a client. A tag
 * can be moved; the content is what was deployed. Empty when the build is
 * not from an image.
 */
export function imageWithContent(
  image: string | null | undefined,
  imageId: string | null | undefined
): string {
  const content = imageId ? `content ${shortImageId(imageId)}` : ''
  // An image uploaded from a client has no reference, only its content.
  if (!image) return content ? `an uploaded image (${content})` : ''
  return content ? `the image ${image} (${content})` : `the image ${image}`
}

export type PluginImageStep = 'service' | 'deploy' | 'live' | 'add'

/** What adding a plugin from an image does, in the order it does it. */
export const PLUGIN_IMAGE_STEPS: { step: PluginImageStep; label: string }[] = [
  { step: 'service', label: 'Create the service' },
  { step: 'deploy', label: 'Deploy the image' },
  { step: 'live', label: 'Wait until it is live' },
  { step: 'add', label: 'Add it as a plugin' },
]

export interface PluginImageValues {
  image: string
  /** Name of the service that is created. */
  name: string
  /** The port the plugin listens on. */
  port: number
}

/**
 * What a run has done so far. A run that failed is tried again with the same
 * object, so a service that was created is not created twice and an image
 * that is live is not deployed again.
 */
export interface PluginImageProgress {
  name?: string
  projectId?: number
  projectSlug?: string
  environmentId?: number
  image?: string
  deploymentId?: number
  live?: boolean
}

/**
 * A refusal this flow decides itself. It carries a problem's `title` and
 * `detail`, so it is announced and shown like a refusal from the server.
 */
export class PluginImageError extends Error {
  readonly title: string
  readonly detail: string
  constructor(title: string, detail: string) {
    super(detail)
    this.name = 'PluginImageError'
    this.title = title
    this.detail = detail
  }
}

export interface PluginImageWait {
  /** Milliseconds between two looks at the deployment. */
  pollMs: number
  /** Milliseconds after which a deployment that is not live is given up on. */
  timeoutMs: number
  sleep: (milliseconds: number) => Promise<void>
}

const DEFAULT_WAIT: PluginImageWait = {
  pollMs: 2_000,
  timeoutMs: 10 * 60 * 1_000,
  sleep: (milliseconds) =>
    new Promise((resolve) => setTimeout(resolve, milliseconds)),
}

export interface AddedPluginFromImage {
  added: AddedServicePluginResponse
  projectSlug: string
  deploymentId: number
}

/**
 * Create a service for a plugin's image, deploy the image, wait until it is
 * live and add the service as a plugin. `progress` is filled as it goes and
 * read first, so calling this again after a failure continues where it
 * stopped.
 */
export async function addPluginFromImage(
  values: PluginImageValues,
  progress: PluginImageProgress,
  onStep: (step: PluginImageStep, progress: PluginImageProgress) => void,
  wait: PluginImageWait = DEFAULT_WAIT
): Promise<AddedPluginFromImage> {
  const report = (step: PluginImageStep) => onStep(step, { ...progress })
  // Another name is another service: nothing done so far belongs to it.
  if (progress.name !== values.name) {
    for (const key of Object.keys(progress) as (keyof PluginImageProgress)[])
      delete progress[key]
    progress.name = values.name
  }
  // Another image is deployed to the service that was made.
  if (progress.image !== values.image) {
    progress.image = values.image
    progress.deploymentId = undefined
    progress.live = undefined
  }

  if (progress.projectId === undefined || progress.projectSlug === undefined) {
    report('service')
    // A plugin's image is never deployed over a service that merely shares
    // its name.
    const existing = await getProjectBySlug({ path: { slug: values.name } })
    if (existing.data)
      throw new PluginImageError(
        'A service with this name exists',
        `A service named ${values.name} already exists. Choose another name, or deploy the image to that service and add it under "From a service".`
      )
    const created = (
      await createProject({
        body: {
          name: values.name,
          preset: 'dockerfile',
          directory: './',
          main_branch: 'main',
          source_type: 'docker_image',
          project_type: 'docker',
          automatic_deploy: false,
          exposed_port: values.port,
        },
        throwOnError: true,
      })
    ).data
    progress.projectId = created.id
    progress.projectSlug = created.slug
  }
  const projectId = progress.projectId
  const projectSlug = progress.projectSlug

  if (progress.environmentId === undefined) {
    report('service')
    const environments = (
      await getEnvironments({
        path: { project_id: projectId },
        throwOnError: true,
      })
    ).data
    const environment =
      environments.find((entry) => entry.name === 'production') ??
      environments[0]
    if (!environment)
      throw new PluginImageError(
        'The service has no environment',
        `The service ${projectSlug} was created without an environment to deploy to. Open the service and add one, then try again.`
      )
    progress.environmentId = environment.id
  }
  const environmentId = progress.environmentId

  if (progress.deploymentId === undefined) {
    report('deploy')
    const started = (
      await deployFromImage({
        path: { project_id: projectId, environment_id: environmentId },
        // A plugin answers its health check on /health unless its manifest
        // says otherwise; an image deployment probes "/" by default.
        body: { image_ref: values.image, health_check_path: '/health' },
        throwOnError: true,
      })
    ).data
    progress.deploymentId = started.id
  }
  const deploymentId = progress.deploymentId

  if (!progress.live) {
    report('live')
    const deadline = Date.now() + wait.timeoutMs
    for (;;) {
      const deployment = (
        await getDeployment({
          path: { project_id: projectId, deployment_id: deploymentId },
          throwOnError: true,
        })
      ).data
      const phase = firstDeployPhase(deployment.status)
      if (phase === 'succeeded') break
      if (phase === 'failed') {
        // Trying again deploys the image again, to the same service.
        progress.deploymentId = undefined
        throw new PluginImageError(
          'The deployment did not become live',
          `Deployment ${deploymentId} of ${projectSlug} ended as ${deployment.status}${deployment.cancelled_reason ? `: ${deployment.cancelled_reason}` : ''}. Nothing was added as a plugin. Open the service to read its logs, then try again.`
        )
      }
      if (Date.now() >= deadline)
        throw new PluginImageError(
          'The deployment is not live yet',
          `Deployment ${deploymentId} of ${projectSlug} is still ${deployment.status}. Nothing was added as a plugin yet. Try again to keep waiting for it.`
        )
      await wait.sleep(wait.pollMs)
    }
    progress.live = true
  }

  report('add')
  const added = (
    await addServicePlugin({
      body: { project_id: projectId, environment_id: environmentId },
      throwOnError: true,
    })
  ).data
  return { added, projectSlug, deploymentId }
}
