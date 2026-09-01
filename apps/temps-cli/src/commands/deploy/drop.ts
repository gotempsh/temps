// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { existsSync, statSync } from 'node:fs'
import { lstat, mkdtemp, readFile, readdir, rm } from 'node:fs/promises'
import { basename, join, resolve } from 'node:path'
import { tmpdir } from 'node:os'
import { spawn } from 'node:child_process'
import {
  createProject,
  deleteProject,
  deployFromUploadedSource,
  getEnvironments,
  getProject,
  getProjectBySlug,
  inspectDropArchive,
  updateProjectSettings,
} from '../../api/sdk.gen.js'
import type { EnvironmentResponse, ProjectResponse, SourceType } from '../../api/types.gen.js'
import { requireAuth, config, credentials } from '../../config/store.js'
import { client, normalizeApiUrl, setupClient } from '../../lib/api-client.js'
import { watchDeployment } from '../../lib/deployment-watcher.jsx'
import { failSpinner, startSpinner, succeedSpinner } from '../../ui/spinner.js'
import { info, success, warning } from '../../ui/output.js'

interface DropOptions {
  name?: string
  preset?: string
  directory?: string
  project?: string
  environment?: string
  wait?: boolean
  timeout?: string
}

interface Candidate {
  directory: string
  preset: string
  label: string
  confidence: string
  reason: string
  isStatic: boolean
  composePath?: string | null
}

interface Inspection {
  suggestedName: string
  candidates: Candidate[]
}

export function slugName(value: string): string {
  const slug = value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 63)
  return slug || `drop-${Math.random().toString(36).slice(2, 10)}`
}

export async function zipDirectory(directory: string): Promise<{ data: Buffer; cleanup: () => Promise<void> }> {
  async function rejectSymlinks(current: string): Promise<void> {
    for (const entry of await readdir(current, { withFileTypes: true })) {
      const path = join(current, entry.name)
      const metadata = await lstat(path)
      if (metadata.isSymbolicLink()) {
        throw new Error(`Drop does not follow symbolic links: ${path}`)
      }
      if (metadata.isDirectory() && !['.git', 'node_modules'].includes(entry.name)) {
        await rejectSymlinks(path)
      }
    }
  }

  await rejectSymlinks(directory)
  const temporaryDirectory = await mkdtemp(join(tmpdir(), 'temps-drop-'))
  const archivePath = join(temporaryDirectory, 'source.zip')
  await new Promise<void>((resolvePromise, reject) => {
    const child = spawn('zip', [
      '-q', '-r', '-y', archivePath, '.',
      '-x', '.git/*', '*/.git/*', 'node_modules/*', '*/node_modules/*',
      '.env', '.env.*', '*/.env', '*/.env.*', '*.pem', '*.key',
      'credentials.json', '*/credentials.json', '.DS_Store', '*/.DS_Store',
    ], { cwd: directory, stdio: 'ignore' })
    child.once('error', reject)
    child.once('exit', (code) => {
      if (code === 0) resolvePromise()
      else reject(new Error(`zip exited with status ${code ?? 'unknown'}`))
    })
  })
  return {
    data: await readFile(archivePath),
    cleanup: () => rm(temporaryDirectory, { recursive: true, force: true }),
  }
}

/**
 * Source types that can receive an uploaded archive. Everything else has a
 * dedicated command, and silently deploying into it would be a surprise.
 */
const UPLOADABLE_SOURCE_TYPES: SourceType[] = ['uploaded_source', 'static_files']

const COMMAND_BY_SOURCE_TYPE: Partial<Record<SourceType, string>> = {
  git: 'deploy',
  docker_image: 'deploy:image',
  manual: 'deploy:image',
}

/** A project's build directory as the API stores it: `.`-relative, no leading `./`. */
export function normalizeDirectory(directory?: string | null): string {
  return (directory || '.').replace(/^\.\/+/, '') || '.'
}

/**
 * Look up an existing project by slug or numeric ID.
 *
 * Only ever called with an explicit `--project` value. It deliberately does
 * NOT go through `resolveProjectSlug()`, which would also consult
 * `.temps/config.json`, `TEMPS_PROJECT` and the context default — picking
 * those up would change what a bare `drop ./` does for anyone who has linked
 * the directory, turning a create into an overwrite without being asked.
 */
async function resolveExistingProject(reference: string): Promise<ProjectResponse> {
  const numericId = /^\d+$/.test(reference) ? Number.parseInt(reference, 10) : null
  const result = numericId === null
    ? await getProjectBySlug({ client, path: { slug: reference } })
    : await getProject({ client, path: { id: numericId } })
  if (result.error || !result.data) {
    throw new Error(`Project "${reference}" not found`)
  }
  return result.data as ProjectResponse
}

/**
 * Reject targets whose deployments come from somewhere other than an upload.
 *
 * A project that has opted into alternate sources is accepted whatever its
 * `source_type` is — that opt-in exists precisely so a Git project can also be
 * shipped from a local folder while keeping its repository. Mirrors the
 * server's check in `deploy_from_uploaded_source`; failing here first just
 * turns a round trip and a 400 into an immediate, more specific message.
 */
export function assertUploadable(
  project: Pick<ProjectResponse, 'slug' | 'source_type' | 'allow_alternate_sources'>,
): void {
  if (UPLOADABLE_SOURCE_TYPES.includes(project.source_type)) return
  if (project.allow_alternate_sources) return
  const command = COMMAND_BY_SOURCE_TYPE[project.source_type]
  const alternative = command
    ? `\nDeploy it from its configured source: bunx @temps-sdk/cli ${command} --project ${project.slug}`
    : ''
  throw new Error(
    `Project "${project.slug}" deploys from ${project.source_type}, ` +
    `so it cannot take an uploaded source archive.\n` +
    `Allow it to: bunx @temps-sdk/cli projects source --project ${project.slug} --allow-alternate` +
    alternative
  )
}

/**
 * Choose which detected project inside the archive to deploy.
 *
 * `--preset`/`--directory` filter, exactly as they always have. When neither
 * is given and we're targeting an existing project, prefer the candidate that
 * matches the project's current build directory so a re-drop of the same
 * folder stays on the same subproject instead of jumping to whichever
 * candidate the server happened to rank first. Mirrors the console's
 * `ProjectDrop` page.
 */
export function selectCandidate(
  candidates: Candidate[],
  options: Pick<DropOptions, 'preset' | 'directory'>,
  currentDirectory?: string,
): Candidate | undefined {
  const matches = candidates.filter((item) =>
    (!options.preset || item.preset === options.preset) &&
    (!options.directory || item.directory === options.directory)
  )
  if (currentDirectory !== undefined && !options.preset && !options.directory) {
    const sameDirectory = matches.find(
      (item) => normalizeDirectory(item.directory) === normalizeDirectory(currentDirectory)
    )
    if (sameDirectory) return sameDirectory
  }
  return matches[0]
}

/**
 * Persist `directory`/`preset` when the archive no longer matches what the
 * project is configured to build — and nothing else. Ports, resource limits,
 * environment variables and domains are left alone: re-uploading source is not
 * an invitation to re-provision the service. Same contract as the console.
 */
async function syncBuildConfig(project: ProjectResponse, candidate: Candidate): Promise<void> {
  const nextDirectory = normalizeDirectory(candidate.directory)
  const changes = buildConfigChanges(project, candidate)
  if (changes.length === 0) return

  startSpinner('Updating build configuration...')
  const updated = await updateProjectSettings({
    client,
    path: { project_id: project.id },
    body: {
      directory: nextDirectory,
      preset: candidate.preset,
      ...(candidate.preset === 'docker-compose' && candidate.composePath
        ? { preset_config: { composePath: candidate.composePath } }
        : {}),
    },
  })
  if (updated.error) throw new Error(JSON.stringify(updated.error))
  succeedSpinner(`Build config updated: ${changes.join(', ')}`)

  // Build config is shared with the project's primary source, so on a project
  // that normally builds from somewhere else (a Git repo, typically) this drop
  // has just changed how the NEXT ordinary deploy builds too. Say so — finding
  // that out from a surprising build later is exactly the kind of silent
  // change the alternate-sources opt-in is meant to keep deliberate.
  if (!UPLOADABLE_SOURCE_TYPES.includes(project.source_type)) {
    warning(
      `These build settings also apply to ${project.slug}'s ${project.source_type} deployments. ` +
      `Re-run \`projects settings\` if the next non-drop deploy should build differently.`
    )
  }
}

/** Human-readable list of what re-detection would change about the project. */
export function buildConfigChanges(
  project: Pick<ProjectResponse, 'directory' | 'preset'>,
  candidate: Pick<Candidate, 'directory' | 'preset'>,
): string[] {
  const changes: string[] = []
  const nextDirectory = normalizeDirectory(candidate.directory)
  const currentDirectory = normalizeDirectory(project.directory)
  if (nextDirectory !== currentDirectory) {
    changes.push(`directory ${currentDirectory} → ${nextDirectory}`)
  }
  if (candidate.preset !== project.preset) {
    changes.push(`preset ${project.preset || 'none'} → ${candidate.preset}`)
  }
  return changes
}

/** Pick the target environment for an existing project, honouring `--environment`. */
export function selectEnvironment(
  environments: EnvironmentResponse[],
  wanted: string | undefined,
  projectSlug: string,
): EnvironmentResponse {
  if (wanted) {
    const named = environments.find((item) => item.name === wanted)
    if (named) return named
    const available = environments.map((item) => item.name).join(', ') || 'none'
    throw new Error(
      `Project "${projectSlug}" has no environment named "${wanted}" (available: ${available})`
    )
  }
  const fallback = environments.find((item) => item.name === 'production') || environments[0]
  if (!fallback) throw new Error(`Project "${projectSlug}" has no environments`)
  return fallback
}

export async function drop(path: string, options: DropOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  if (options.environment && !options.project) {
    throw new Error('--environment requires --project: a newly created project only has production')
  }
  const sourcePath = resolve(path)
  if (!existsSync(sourcePath)) throw new Error(`Path does not exist: ${sourcePath}`)

  const isDirectory = statSync(sourcePath).isDirectory()
  if (!isDirectory && !sourcePath.toLowerCase().endsWith('.zip')) {
    throw new Error('Source drop accepts a directory or .zip archive')
  }

  startSpinner('Preparing source archive...')
  let cleanup = async () => {}
  let data: Buffer
  if (isDirectory) {
    const prepared = await zipDirectory(sourcePath)
    data = prepared.data
    cleanup = prepared.cleanup
  } else {
    data = await readFile(sourcePath)
  }

  const apiUrl = normalizeApiUrl(config.get('apiUrl'))
  const apiKey = await credentials.getApiKey()
  const headers = { Authorization: `Bearer ${apiKey}` }
  const archive = new Blob([new Uint8Array(data)], { type: 'application/zip' })

  let projectId: number | undefined
  try {
    succeedSpinner(`Prepared ${(data.length / 1024 / 1024).toFixed(2)} MiB ZIP`)
    startSpinner('Detecting project preset...')
    const inspected = await inspectDropArchive({
      client,
      body: {
        file: new File(
          [archive],
          isDirectory ? `${basename(sourcePath)}.zip` : basename(sourcePath),
          { type: 'application/zip' },
        ),
      },
    })
    if (inspected.error || !inspected.data) {
      throw new Error(JSON.stringify(inspected.error || 'Preset inspection returned no data'))
    }
    const inspection = inspected.data as Inspection

    // `--project` targets a project that already exists; without it the
    // command creates one, exactly as it always has.
    const existing = options.project ? await resolveExistingProject(options.project) : null
    if (existing) assertUploadable(existing)

    const candidate = selectCandidate(
      inspection.candidates,
      options,
      existing ? existing.directory : undefined,
    )
    if (!candidate) throw new Error('No detected project matches --preset/--directory')
    succeedSpinner(`${candidate.label} (${candidate.confidence}): ${candidate.reason}`)

    let target: { id: number; slug: string }
    let environment: EnvironmentResponse
    if (existing) {
      await syncBuildConfig(existing, candidate)
      target = existing

      const environments = await getEnvironments({ client, path: { project_id: existing.id } })
      const environmentList = Array.isArray(environments.data) ? environments.data : []
      environment = selectEnvironment(environmentList, options.environment, existing.slug)
      info(`Deploying into ${existing.slug} (${environment.name})`)
    } else {
      const name = slugName(options.name || inspection.suggestedName || basename(sourcePath))
      startSpinner(`Creating project ${name}...`)
      const created = await createProject({
        client,
        body: {
          name,
          directory: candidate.directory,
          main_branch: 'main',
          preset: candidate.preset,
          source_type: candidate.isStatic ? 'static_files' : 'uploaded_source',
          automatic_deploy: false,
          storage_service_ids: [],
          preset_config:
            candidate.preset === 'docker-compose' && candidate.composePath
              ? { composePath: candidate.composePath }
              : undefined,
        },
      })
      if (created.error || !created.data) throw new Error(JSON.stringify(created.error || 'Project creation returned no data'))
      // Set only for projects this command created — the rollback below must
      // never delete a project the user already had.
      projectId = created.data.id
      target = created.data

      const environments = await getEnvironments({ client, path: { project_id: projectId } })
      const environmentList = Array.isArray(environments.data) ? environments.data : []
      const firstEnvironment = environmentList.find((item) => item.name === 'production') || environmentList[0]
      if (!firstEnvironment) throw new Error('Created project has no environment')
      environment = firstEnvironment
      succeedSpinner(`Created ${created.data.slug}`)
    }

    startSpinner(candidate.isStatic ? 'Uploading static site...' : 'Uploading source and starting deployment...')
    const deployForm = new FormData()
    deployForm.append('file', archive, 'source.zip')
    let deployment: { id: number; slug: string }
    if (candidate.isStatic) {
      const upload = await fetch(`${apiUrl}/projects/${target.id}/upload/static`, {
        method: 'POST', headers, body: deployForm,
      })
      if (!upload.ok) throw new Error(await upload.text())
      const bundle = (await upload.json()) as { id: number }
      const response = await fetch(
        `${apiUrl}/projects/${target.id}/environments/${environment.id}/deploy/static`,
        {
          method: 'POST',
          headers: { ...headers, 'Content-Type': 'application/json' },
          body: JSON.stringify({ static_bundle_id: bundle.id }),
        },
      )
      if (!response.ok) throw new Error(await response.text())
      deployment = (await response.json()) as { id: number; slug: string }
    } else {
      const sourceDeployment = await deployFromUploadedSource({
        client,
        path: { project_id: target.id, environment_id: environment.id },
        body: { file: new File([archive], 'source.zip', { type: 'application/zip' }) },
      })
      if (sourceDeployment.error || !sourceDeployment.data) {
        throw new Error(JSON.stringify(sourceDeployment.error || 'Deployment returned no data'))
      }
      deployment = sourceDeployment.data
    }
    succeedSpinner(`Deployment started: ${deployment.slug}`)
    info(`Dashboard: ${config.get('apiUrl').replace(/\/api\/?$/, '')}/projects/${target.slug}/deployments/${deployment.id}`)
    if (options.wait !== false) {
      const result = await watchDeployment({
        projectId: target.id,
        deploymentId: deployment.id,
        timeoutSecs: Number(options.timeout || '600'),
        projectName: target.slug,
      })
      if (!result.success) process.exitCode = 1
    } else {
      success('Source deployment is running in the background.')
    }
  } catch (error) {
    failSpinner('Drop failed')
    if (projectId) {
      const deleted = await deleteProject({ client, path: { id: projectId } })
      if (deleted.error) warning(`Could not remove incomplete project ${projectId}`)
      else info(`Removed incomplete project ${projectId}`)
    }
    throw error
  } finally {
    await cleanup()
  }
}
