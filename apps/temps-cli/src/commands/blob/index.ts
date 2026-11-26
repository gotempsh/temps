// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps blob`: a project's files in Temps Blob storage (the store
 * `@temps-sdk/blob` reads), and the instance-wide S3-compatible service
 * behind it.
 */

import type { Command } from 'commander'
import { basename } from 'node:path'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  blobCopy,
  blobDelete,
  blobDisable,
  blobEnable,
  blobGetSettings,
  blobList,
  blobPut,
  blobStatus,
  blobUpdate,
  blobUpdateSettings,
} from '../../api/sdk.gen.js'
import type { BlobResponse, EnableBlobRequest, ListBlobsResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue, formatRelativeTime } from '../../ui/output.js'
import { ValidationError } from '../../utils/errors.js'
import { parseIntegerFlag, resolveProjectFlag } from '../../lib/project-id.js'
import { addCredentialSources } from '../../lib/value-source.js'
import { containerNameRequest, printContainerNameSettings, printManagedServiceStatus } from '../../lib/managed-service.js'

const PROJECT_FLAG = '-p, --project <slug-or-id>'
const PROJECT_HELP = 'Project slug or ID (defaults to the linked project)'

/** `blob list --all` stops after this many pages, so a huge store cannot run it forever. */
export const MAX_LIST_PAGES = 100

interface ProjectOptions {
  project?: string
  json?: boolean
}

interface ListOptions extends ProjectOptions {
  prefix?: string
  limit?: string
  cursor?: string
  all?: boolean
}

interface PutOptions extends ProjectOptions {
  pathname?: string
  contentType?: string
  randomSuffix?: boolean
}

interface GetOptions extends ProjectOptions {
  output?: string
}

interface ConfirmOptions {
  yes?: boolean
}

export interface EnableOptions {
  image?: string
  rootUser?: string
  rootPassword?: string
  json?: boolean
}

function fail(what: string, error: unknown): never {
  throw new Error(`${what}: ${getErrorMessage(error)}`)
}

/** A stored pathname: relative to the project, without a leading slash. */
export function normalizePathname(raw: string): string {
  const pathname = raw.trim().replace(/^\/+/, '')
  if (!pathname) throw new ValidationError('A blob pathname cannot be empty')
  if (pathname.split('/').some((segment) => segment === '..')) {
    throw new ValidationError(`A blob pathname cannot contain "..": "${raw}"`)
  }
  return pathname
}

/**
 * The download/metadata URL of a blob. The server route takes the rest of the
 * path (`{*path}`), so slashes stay literal and each segment is encoded:
 * `/blob/7/docs/my%20file.txt`, never `docs%2Fmy%20file.txt`, which proxies
 * may refuse or rewrite. Same form as `@temps-sdk/blob`.
 */
export function blobObjectUrl(projectId: number, pathname: string): string {
  const encoded = pathname.split('/').map(encodeURIComponent).join('/')
  return `/blob/${projectId}/${encoded}`
}

/** The enable request from flags; every field is optional. */
export function buildEnableRequest(options: EnableOptions): EnableBlobRequest {
  const body: EnableBlobRequest = {}
  if (options.image !== undefined) body.docker_image = options.image
  if (options.rootUser !== undefined) body.root_user = options.rootUser
  if (options.rootPassword !== undefined) body.root_password = options.rootPassword
  if ((body.root_user === undefined) !== (body.root_password === undefined)) {
    throw new ValidationError('Pass --root-user and a root password together, or neither')
  }
  return body
}

/** The metadata `blob head` reads from the response headers. */
export function blobMetadata(pathname: string, headers: Headers): Record<string, string | number | null> {
  const length = headers.get('content-length')
  return {
    pathname,
    contentType: headers.get('content-type'),
    size: length === null ? null : Number(length),
    lastModified: headers.get('last-modified'),
    etag: headers.get('etag'),
    cacheControl: headers.get('cache-control'),
  }
}

export function registerBlobCommands(program: Command): void {
  const blob = program
    .command('blob')
    .description("Upload, download and list a project's files in Blob storage, and manage the Blob service")

  blob.command('list [prefix]')
    .alias('ls')
    .description('List blobs, optionally under a pathname prefix')
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('--limit <n>', 'Blobs per page')
    .option('--cursor <token>', 'Continue from the cursor a previous page returned')
    .option('--all', `Follow cursors to list everything (at most ${MAX_LIST_PAGES} pages)`)
    .option('--json', 'Output in JSON format')
    .action(listAction)

  blob.command('put <file>')
    .alias('upload')
    .description('Upload a local file')
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('--pathname <path>', 'Where to store it (default: the file name)')
    .option('--content-type <type>', 'Content type (default: guessed from the extension)')
    .option('--random-suffix', 'Add a random suffix to the pathname so it never overwrites')
    .option('--json', 'Output in JSON format')
    .action(putAction)

  blob.command('get <pathname>')
    .alias('download')
    .description('Download a blob to a file, or to stdout with -o -')
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('-o, --output <path>', 'Where to write it (default: its file name in this directory; - for stdout)')
    .action(getAction)

  blob.command('head <pathname>')
    .alias('stat')
    .description("Show a blob's metadata (size, content type, last modified) without downloading it")
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('--json', 'Output in JSON format')
    .action(headAction)

  blob.command('copy <from> <to>')
    .alias('cp')
    .description('Copy a blob (by pathname or URL) to a new pathname')
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('--json', 'Output in JSON format')
    .action(copyAction)

  blob.command('del <pathnames...>')
    .alias('delete')
    .description('Delete one or more blobs')
    .option(PROJECT_FLAG, PROJECT_HELP)
    .option('-y, --yes', 'Do not ask for confirmation')
    .option('--json', 'Output in JSON format')
    .action(delAction)

  // The instance-wide service.
  blob.command('status')
    .description('Show whether the Blob service is enabled and healthy on this instance')
    .option('--json', 'Output in JSON format')
    .action(statusAction)

  const enable = blob
    .command('enable')
    .description('Enable the Blob service on this instance (starts its S3-compatible container)')
    .option('--image <ref>', 'Docker image to run (default: the one this Temps release pins)')
    .option('--root-user <user>', 'Root user for S3 access (default: generated)')
    .option('--root-password <password>', 'Root password for S3 access (prefer --root-password-stdin)')
    .option('--json', 'Output in JSON format')
  addCredentialSources(enable, [{ flag: 'root-password', what: 'the root password' }])
  enable.action(enableAction)

  blob.command('update')
    .description('Change the Docker image the Blob service runs (recreates its container)')
    .requiredOption('--image <ref>', 'Docker image to run')
    .option('-y, --yes', 'Do not ask for confirmation')
    .option('--json', 'Output in JSON format')
    .action(updateAction)

  blob.command('disable')
    .description('Disable the Blob service on this instance (stops its container; every project loses Blob access)')
    .option('-y, --yes', 'Do not ask for confirmation')
    .option('--json', 'Output in JSON format')
    .action(disableAction)

  blob.command('container-name')
    .description('Show, or set with --container-name/--reset, the Docker container name of the Blob service')
    .option('--container-name <name>', 'New container name (must keep the required prefix)')
    .option('--reset', 'Go back to the default container name')
    .option('--json', 'Output in JSON format')
    .action(containerNameAction)
}

async function prepare(project: string | undefined): Promise<number> {
  await requireAuth()
  await setupClient()
  return (await resolveProjectFlag(project)).id
}

async function listPage(projectId: number, prefix: string | undefined, limit: number | undefined, cursor: string | undefined): Promise<ListBlobsResponse> {
  const { data, error } = await blobList({
    client,
    query: {
      project_id: projectId,
      ...(prefix !== undefined && { prefix }),
      ...(limit !== undefined && { limit }),
      ...(cursor !== undefined && { cursor }),
    },
  })
  if (error || !data) fail(`Could not list blobs${prefix ? ` under "${prefix}"` : ''}`, error)
  return data
}

async function listAction(prefix: string | undefined, options: ListOptions): Promise<void> {
  const limit = options.limit === undefined ? undefined : parseIntegerFlag(options.limit, '--limit', 1)
  const projectId = await prepare(options.project)
  const result = await withSpinner('Listing blobs...', async () => {
    let page = await listPage(projectId, prefix, limit, options.cursor)
    if (!options.all) return page
    const blobs = [...page.blobs]
    let pages = 1
    while (page.hasMore && page.cursor && pages < MAX_LIST_PAGES) {
      page = await listPage(projectId, prefix, limit, page.cursor)
      blobs.push(...page.blobs)
      pages++
    }
    return { blobs, hasMore: page.hasMore, cursor: page.cursor }
  })
  if (options.json) {
    json(result)
    return
  }
  newline()
  header(`${icons.info} Blobs${prefix ? ` under ${colors.bold(prefix)}` : ''} (${result.blobs.length})`)
  if (result.blobs.length === 0) {
    info('No blobs. Upload one with: temps blob put <file>')
  } else {
    const columns: TableColumn<BlobResponse>[] = [
      { header: 'Pathname', key: 'pathname', color: (v) => colors.bold(v) },
      { header: 'Size', accessor: (b) => String(b.size) },
      { header: 'Type', key: 'contentType' },
      { header: 'Uploaded', accessor: (b) => formatRelativeTime(b.uploadedAt) },
    ]
    printTable(result.blobs, columns, { style: 'minimal' })
  }
  if (result.hasMore && result.cursor) info(`More blobs: temps blob list --cursor ${result.cursor}`)
  newline()
}

async function putAction(file: string, options: PutOptions): Promise<void> {
  const source = Bun.file(file)
  if (!(await source.exists())) throw new ValidationError(`File not found: ${file}`)
  const pathname = normalizePathname(options.pathname ?? basename(file))
  const projectId = await prepare(options.project)
  const result = await withSpinner(`Uploading ${pathname}...`, async () => {
    const { data, error } = await blobPut({
      client,
      // The SDK types an octet-stream body as a string; a Blob streams the file.
      body: source as unknown as string,
      query: {
        project_id: projectId,
        pathname,
        ...(options.contentType !== undefined && { content_type: options.contentType }),
        ...(options.randomSuffix && { add_random_suffix: true }),
      },
    })
    if (error || !data) fail(`Could not upload ${file} to ${pathname}`, error)
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(`Uploaded ${result.pathname} (${result.size} bytes, ${result.contentType})`)
  keyValue('URL', result.url)
}

async function getAction(rawPathname: string, options: GetOptions): Promise<void> {
  const pathname = normalizePathname(rawPathname)
  const output = options.output ?? basename(pathname)
  const projectId = await prepare(options.project)
  // The generated blobDownload would encode the slashes in `path` (see blobObjectUrl).
  // api-coverage: GET /blob/{project_id}/{path}
  const { data, error, response } = await client.get({ url: blobObjectUrl(projectId, pathname), parseAs: 'stream' })
  if (error || !response?.ok) fail(`Could not download ${pathname}`, error)
  const body = new Response(data as ReadableStream | null)
  if (output === '-') {
    await Bun.write(Bun.stdout, body)
    return
  }
  const written = await Bun.write(output, body)
  success(`Saved ${pathname} to ${output} (${written} bytes)`)
}

async function headAction(rawPathname: string, options: ProjectOptions): Promise<void> {
  const pathname = normalizePathname(rawPathname)
  const projectId = await prepare(options.project)
  // api-coverage: HEAD /blob/{project_id}/{path}
  const { error, response } = await client.head({ url: blobObjectUrl(projectId, pathname) })
  if (response?.status === 404) {
    warning(`Blob "${pathname}" does not exist`)
    process.exitCode = 1
    return
  }
  if (error || !response?.ok) fail(`Could not read the metadata of ${pathname}`, error)
  const metadata = blobMetadata(pathname, response.headers)
  if (options.json) {
    json(metadata)
    return
  }
  newline()
  header(`${icons.info} ${pathname}`)
  for (const [key, value] of Object.entries(metadata)) if (key !== 'pathname') keyValue(key, value ?? '-')
  newline()
}

async function copyAction(from: string, rawTo: string, options: ProjectOptions): Promise<void> {
  const toPathname = normalizePathname(rawTo)
  const projectId = await prepare(options.project)
  const result = await withSpinner(`Copying ${from} to ${toPathname}...`, async () => {
    const { data, error } = await blobCopy({ client, body: { fromUrl: from, toPathname, projectId } })
    if (error || !data) fail(`Could not copy ${from} to ${toPathname}`, error)
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(`Copied to ${result.pathname}`)
}

async function delAction(rawPathnames: string[], options: ProjectOptions & ConfirmOptions): Promise<void> {
  const pathnames = rawPathnames.map(normalizePathname)
  const projectId = await prepare(options.project)
  if (!options.yes) {
    const what = pathnames.length === 1 ? `"${pathnames[0]}"` : `${pathnames.length} blobs`
    if (!(await promptConfirm({ message: `Delete ${what}? This cannot be undone.`, default: false }))) {
      info('Cancelled; nothing was deleted.')
      return
    }
  }
  const result = await withSpinner('Deleting...', async () => {
    const { data, error } = await blobDelete({ client, body: { pathnames, projectId } })
    if (error || !data) fail(`Could not delete ${pathnames.join(', ')}`, error)
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(`Deleted ${result.deleted} of ${pathnames.length} blob(s)`)
}

async function statusAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()
  const status = await withSpinner('Fetching Blob status...', async () => {
    const { data, error } = await blobStatus({ client })
    if (error || !data) fail('Could not read the Blob service status', error)
    return data
  })
  if (options.json) {
    json(status)
    return
  }
  printManagedServiceStatus('Blob service', 'blob', status)
}

async function enableAction(options: EnableOptions): Promise<void> {
  const body = buildEnableRequest(options)
  await requireAuth()
  await setupClient()
  const result = await withSpinner('Enabling the Blob service...', async () => {
    const { data, error } = await blobEnable({ client, body })
    if (error || !data) fail('Could not enable the Blob service', error)
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(result.message)
  info('Check it with: temps blob status')
}

async function updateAction(options: { image: string; json?: boolean } & ConfirmOptions): Promise<void> {
  const image = options.image.trim()
  if (!image) throw new ValidationError('--image needs an image reference')
  await requireAuth()
  await setupClient()
  if (!options.yes) {
    warning('Changing the image recreates the Blob container; projects cannot reach Blob storage until it is back.')
    if (!(await promptConfirm({ message: `Run the Blob service on ${image}?`, default: false }))) {
      info('Cancelled; nothing was changed.')
      return
    }
  }
  const result = await withSpinner(`Updating the Blob service to ${image}...`, async () => {
    const { data, error } = await blobUpdate({ client, body: { docker_image: image } })
    if (error || !data) fail(`Could not update the Blob service to ${image}`, error)
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(result.message)
}

async function disableAction(options: ConfirmOptions & { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()
  if (!options.yes) {
    warning('Disabling stops the Blob service: every project using @temps-sdk/blob loses access until it is enabled again.')
    if (!(await promptConfirm({ message: 'Disable the Blob service?', default: false }))) {
      info('Cancelled; nothing was changed.')
      return
    }
  }
  const result = await withSpinner('Disabling the Blob service...', async () => {
    const { data, error } = await blobDisable({ client })
    if (error || !data) fail('Could not disable the Blob service', error)
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(result.message)
}

async function containerNameAction(options: { containerName?: string; reset?: boolean; json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()
  const { data: current, error: readError } = await blobGetSettings({ client })
  if (readError || !current) fail('Could not read the Blob container-name setting', readError)
  if (options.containerName === undefined && !options.reset) {
    if (options.json) json(current)
    else printContainerNameSettings('Blob container name', current)
    return
  }
  const body = containerNameRequest(options, current.required_prefix)
  const saved = await withSpinner('Saving the Blob container name...', async () => {
    const { data, error } = await blobUpdateSettings({ client, body })
    if (error || !data) fail('Could not save the Blob container name', error)
    return data
  })
  if (options.json) {
    json(saved)
    return
  }
  success(`Blob container name: ${saved.container_name}`)
}
