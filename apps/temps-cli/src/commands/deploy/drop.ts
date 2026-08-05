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
  inspectDropArchive,
} from '../../api/sdk.gen.js'
import { requireAuth, config, credentials } from '../../config/store.js'
import { client, normalizeApiUrl, setupClient } from '../../lib/api-client.js'
import { watchDeployment } from '../../lib/deployment-watcher.jsx'
import { failSpinner, startSpinner, succeedSpinner } from '../../ui/spinner.js'
import { info, success, warning } from '../../ui/output.js'

interface DropOptions {
  name?: string
  preset?: string
  directory?: string
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

export async function drop(path: string, options: DropOptions): Promise<void> {
  await requireAuth()
  await setupClient()
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
    const candidate = inspection.candidates.find((item) =>
      (!options.preset || item.preset === options.preset) &&
      (!options.directory || item.directory === options.directory)
    )
    if (!candidate) throw new Error('No detected project matches --preset/--directory')
    succeedSpinner(`${candidate.label} (${candidate.confidence}): ${candidate.reason}`)

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
      },
    })
    if (created.error || !created.data) throw new Error(JSON.stringify(created.error || 'Project creation returned no data'))
    projectId = created.data.id

    const environments = await getEnvironments({ client, path: { project_id: projectId } })
    const environmentList = Array.isArray(environments.data) ? environments.data : []
    const environment = environmentList.find((item) => item.name === 'production') || environmentList[0]
    if (!environment) throw new Error('Created project has no environment')
    succeedSpinner(`Created ${created.data.slug}`)

    startSpinner(candidate.isStatic ? 'Uploading static site...' : 'Uploading source and starting deployment...')
    const deployForm = new FormData()
    deployForm.append('file', archive, 'source.zip')
    let deployment: { id: number; slug: string }
    if (candidate.isStatic) {
      const upload = await fetch(`${apiUrl}/projects/${projectId}/upload/static`, {
        method: 'POST', headers, body: deployForm,
      })
      if (!upload.ok) throw new Error(await upload.text())
      const bundle = (await upload.json()) as { id: number }
      const response = await fetch(
        `${apiUrl}/projects/${projectId}/environments/${environment.id}/deploy/static`,
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
        path: { project_id: projectId, environment_id: environment.id },
        body: { file: new File([archive], 'source.zip', { type: 'application/zip' }) },
      })
      if (sourceDeployment.error || !sourceDeployment.data) {
        throw new Error(JSON.stringify(sourceDeployment.error || 'Deployment returned no data'))
      }
      deployment = sourceDeployment.data
    }
    succeedSpinner(`Deployment started: ${deployment.slug}`)
    info(`Dashboard: ${config.get('apiUrl').replace(/\/api\/?$/, '')}/projects/${created.data.slug}/deployments/${deployment.id}`)
    if (options.wait !== false) {
      const result = await watchDeployment({
        projectId,
        deploymentId: deployment.id,
        timeoutSecs: Number(options.timeout || '600'),
        projectName: created.data.slug,
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
