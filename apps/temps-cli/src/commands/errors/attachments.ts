// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `temps errors attachments`: files SDKs send with errors and feedback,
// kept in the project's blob storage (ADR 111, D4).

import type { Command } from 'commander'
import { mkdir, writeFile } from 'node:fs/promises'
import { basename, dirname } from 'node:path'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  deleteErrorAttachment,
  getErrorAttachmentContent,
  getErrorAttachmentUsage,
  listErrorEventAttachments,
  listFeedbackAttachments,
  setErrorAttachmentQuota,
} from '../../api/sdk.gen.js'
import type { AttachmentUsage } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { header, info, json, keyValue, success, warning } from '../../ui/output.js'
import { promptConfirm } from '../../ui/prompts.js'
import { formatBytes } from '../server/index.js'

/** Largest quota the server accepts: 100 GiB, in MiB. */
export const MAX_QUOTA_MIB = 100 * 1024
const MIB = 1024 * 1024

export const ATTACHMENT_EXAMPLE =
  "Sentry.getCurrentScope().addAttachment({ filename: 'app.log', data: logText })"

type Owner =
  | { kind: 'event'; groupId: number; eventId: number }
  | { kind: 'feedback'; feedbackId: number }

function wholeId(raw: string | undefined, flag: string): number {
  const id = Number(raw)
  if (raw === undefined || !Number.isInteger(id) || id <= 0) {
    throw new Error(`${flag} must be a positive whole number, got "${raw ?? ''}"`)
  }
  return id
}

/** The event (`--group-id` and `--event-id`) or the feedback (`--feedback-id`). */
export function parseOwner(options: { groupId?: string; eventId?: string; feedbackId?: string }): Owner {
  const event = options.groupId !== undefined || options.eventId !== undefined
  if (event === (options.feedbackId !== undefined)) {
    throw new Error('Give either --group-id with --event-id, or --feedback-id')
  }
  if (options.feedbackId !== undefined) {
    return { kind: 'feedback', feedbackId: wholeId(options.feedbackId, '--feedback-id') }
  }
  return {
    kind: 'event',
    groupId: wholeId(options.groupId, '--group-id'),
    eventId: wholeId(options.eventId, '--event-id'),
  }
}

/**
 * Write a downloaded attachment to `path`. Without `force` the file is
 * created only if it does not exist, in one step, so a file that appears
 * meanwhile (another download of the same name) is never replaced: returns
 * false then. With `force` an existing file is replaced.
 */
export async function saveDownload(path: string, bytes: Uint8Array, force: boolean): Promise<boolean> {
  if (force) {
    await Bun.write(path, bytes)
    return true
  }
  try {
    // Like Bun.write in the --force branch, create a missing parent directory.
    await mkdir(dirname(path), { recursive: true })
    // Bun.write has no create-only mode; the `wx` flag is O_CREAT | O_EXCL.
    await writeFile(path, bytes, { flag: 'wx' })
    return true
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'EEXIST') return false
    throw error
  }
}

/** A `--mib` quota: a whole number of MiB from 0 to 100 GiB. */
export function parseQuotaMib(raw: string): number {
  // `Number('')` and `Number(' ')` are 0: only digits are a quota.
  const mib = /^\d+$/.test(raw) ? Number(raw) : Number.NaN
  if (!Number.isInteger(mib) || mib < 0 || mib > MAX_QUOTA_MIB) {
    throw new Error(`--mib must be a whole number from 0 to ${MAX_QUOTA_MIB} (100 GiB), got "${raw}"`)
  }
  return mib
}

/**
 * The file name a download is saved as: the exact name from
 * `filename*=UTF-8''...`, else the quoted fallback, without directories.
 */
export function filenameFromDisposition(disposition: string | null, fallback: string): string {
  const encoded = disposition?.match(/filename\*=UTF-8''([^;]+)/i)?.[1]
  const quoted = disposition?.match(/filename="([^"]*)"/i)?.[1]
  let name = quoted ?? ''
  if (encoded) {
    try {
      name = decodeURIComponent(encoded)
    } catch {
      // A malformed encoding keeps the quoted fallback.
    }
  }
  const clean = basename(name.replace(/\\/g, '/')).trim()
  return clean && clean !== '.' && clean !== '..' ? clean : fallback
}

function printUsage(usage: AttachmentUsage): void {
  if (!usage.configured) {
    warning(usage.reason ?? 'No blob storage is set up, so attachments sent by SDKs are dropped')
    info(`Enable Blob storage in the console (${usage.setup_path ?? '/storage'}) to keep them`)
  }
  keyValue('Used', `${formatBytes(usage.used_bytes)} of ${formatBytes(usage.quota_bytes)}`)
  keyValue('Files', usage.files.toLocaleString())
  keyValue(
    'Limits',
    `${formatBytes(usage.max_attachment_bytes)} per file, ${formatBytes(usage.max_event_attachment_bytes)} or ${usage.max_attachments_per_event} files per event`,
  )
  if (usage.dropped_over_quota > 0 || usage.dropped_queue_full > 0) {
    warning(
      `Since the server started, across projects: ${usage.dropped_over_quota} dropped over quota, ${usage.dropped_queue_full} dropped while the upload queue was full`,
    )
  }
}

interface ProjectOptions {
  projectId: string
  json?: boolean
}

export function registerAttachmentCommands(errors: Command): void {
  const attachments = errors
    .command('attachments')
    .description('Files SDKs send with errors and feedback, kept in blob storage')

  attachments
    .command('list')
    .alias('ls')
    .description('List the attachments of an error event (--group-id with --event-id) or of a feedback (--feedback-id)')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--group-id <id>', 'Error group of the event; give it with --event-id, not with --feedback-id')
    .option('--event-id <id>', 'Error event ID; give it with --group-id, not with --feedback-id')
    .option('--feedback-id <id>', 'Feedback ID; give it alone, instead of --group-id and --event-id')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  attachments
    .command('download')
    .description('Save an attachment to a file')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--attachment-id <id>', 'Attachment ID')
    .option('-o, --output <path>', 'File to write (default: the attachment name, in this directory)')
    .option('-f, --force', 'Overwrite the file if it exists')
    .action(downloadAction)

  attachments
    .command('delete')
    .alias('rm')
    .description('Delete an attachment and return its space to the quota')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--attachment-id <id>', 'Attachment ID')
    .option('-y, --yes', 'Skip confirmation')
    .action(deleteAction)

  attachments
    .command('usage')
    .description('Show whether attachments are kept and how much of the quota they use')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(usageAction)

  attachments
    .command('quota')
    .description('Set how much attachment storage the project may use')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--mib <n>', `Quota in MiB, 0 to ${MAX_QUOTA_MIB}`)
    .option('--json', 'Output in JSON format')
    .action(quotaAction)
}

async function listAction(
  options: ProjectOptions & { groupId?: string; eventId?: string; feedbackId?: string },
): Promise<void> {
  const projectId = wholeId(options.projectId, '--project-id')
  const owner = parseOwner(options)
  await requireAuth()
  await setupClient()
  const items = await withSpinner('Loading attachments...', async () => {
    const { data, error } =
      owner.kind === 'event'
        ? await listErrorEventAttachments({
            client,
            path: { project_id: projectId, group_id: owner.groupId, event_id: owner.eventId },
          })
        : await listFeedbackAttachments({
            client,
            path: { project_id: projectId, feedback_id: owner.feedbackId },
          })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data.data
  })
  if (options.json) {
    json(items)
    return
  }
  header(owner.kind === 'event' ? `Attachments of event #${owner.eventId}` : `Attachments of feedback #${owner.feedbackId}`)
  if (items.length === 0) {
    info(`No files were sent. An SDK attaches one with: ${ATTACHMENT_EXAMPLE}`)
    return
  }
  printTable(items, [
    { header: 'ID', accessor: (row) => String(row.id) },
    { header: 'File', accessor: (row) => row.filename },
    { header: 'Type', accessor: (row) => row.content_type },
    { header: 'Size', accessor: (row) => formatBytes(row.size_bytes) },
  ])
  info(`Save one with: temps errors attachments download --project-id ${projectId} --attachment-id <id>`)
}

async function downloadAction(options: {
  projectId: string
  attachmentId: string
  output?: string
  force?: boolean
}): Promise<void> {
  const projectId = wholeId(options.projectId, '--project-id')
  const attachmentId = wholeId(options.attachmentId, '--attachment-id')
  await requireAuth()
  await setupClient()
  const { bytes, name } = await withSpinner('Downloading attachment...', async () => {
    const { data, error, response } = await getErrorAttachmentContent({
      client,
      path: { project_id: projectId, attachment_id: attachmentId },
      parseAs: 'blob',
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return {
      bytes: new Uint8Array(await data.arrayBuffer()),
      name: filenameFromDisposition(
        response?.headers.get('content-disposition') ?? null,
        `attachment-${attachmentId}`,
      ),
    }
  })
  const output = options.output ?? name
  if (!(await saveDownload(output, bytes, options.force ?? false))) {
    warning(`${output} exists; pass --force to overwrite it or --output to choose another file`)
    return
  }
  success(`Saved ${formatBytes(bytes.byteLength)} to ${output}`)
}

async function deleteAction(options: { projectId: string; attachmentId: string; yes?: boolean }): Promise<void> {
  const projectId = wholeId(options.projectId, '--project-id')
  const attachmentId = wholeId(options.attachmentId, '--attachment-id')
  await requireAuth()
  await setupClient()
  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Delete attachment #${attachmentId}? The file is deleted from blob storage.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  await withSpinner(`Deleting attachment #${attachmentId}...`, async () => {
    const { error } = await deleteErrorAttachment({
      client,
      path: { project_id: projectId, attachment_id: attachmentId },
    })
    if (error) throw new Error(getErrorMessage(error))
  })
  success(`Attachment #${attachmentId} deleted; its space is back in the quota`)
}

async function usageAction(options: ProjectOptions): Promise<void> {
  const projectId = wholeId(options.projectId, '--project-id')
  await requireAuth()
  await setupClient()
  const usage = await withSpinner('Loading attachment usage...', async () => {
    const { data, error } = await getErrorAttachmentUsage({ client, path: { project_id: projectId } })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(usage)
    return
  }
  header('Attachments')
  printUsage(usage)
}

async function quotaAction(options: ProjectOptions & { mib: string }): Promise<void> {
  const projectId = wholeId(options.projectId, '--project-id')
  const mib = parseQuotaMib(options.mib)
  await requireAuth()
  await setupClient()
  const usage = await withSpinner('Setting the attachment quota...', async () => {
    const { data, error } = await setErrorAttachmentQuota({
      client,
      path: { project_id: projectId },
      body: { quota_bytes: mib * MIB },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(usage)
    return
  }
  success(`Attachment quota set to ${formatBytes(usage.quota_bytes)}`)
  printUsage(usage)
}
