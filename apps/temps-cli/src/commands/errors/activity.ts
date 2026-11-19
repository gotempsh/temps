// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `temps errors comment` and `temps errors activity`: comments on an error
// group and its history (ADR 074, D1.7).

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  createErrorGroupComment,
  deleteErrorGroupComment,
  listErrorGroupActivity,
  updateErrorGroupComment,
} from '../../api/sdk.gen.js'
import type { ErrorGroupActivityResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { header, info, json, success, warning, formatRelativeTime } from '../../ui/output.js'

/** Longest comment the server accepts, in characters. */
export const MAX_COMMENT_CHARS = 10_000

/** Trim a comment and check it with the server's rules. */
export function checkCommentBody(raw: string | undefined): string {
  const body = raw?.trim() ?? ''
  if (!body) {
    throw new Error('A comment needs text: pass --body "what you found"')
  }
  if ([...body].length > MAX_COMMENT_CHARS) {
    throw new Error(`A comment is at most ${MAX_COMMENT_CHARS} characters`)
  }
  return body
}

/** One line for an activity entry, without its actor or time. */
export function describeActivityEntry(entry: ErrorGroupActivityResponse): string {
  const d = entry.details ?? {}
  const groups = (d.group_ids ?? []).map((id) => `#${id}`).join(', ') || 'another group'
  const moved = d.fingerprints != null ? ` (${d.fingerprints} fingerprint${d.fingerprints === 1 ? '' : 's'})` : ''
  switch (entry.kind) {
    case 'comment':
      return entry.body ?? ''
    case 'status_changed':
      return `set status ${d.status ?? '?'}${d.in_next_release ? ' in the next release' : ''}${d.bulk ? ' (bulk)' : ''}`
    case 'regressed':
      return `reopened by a new event${d.release ? ` in release ${d.release}` : ''} (was ${d.status ?? 'resolved'})`
    case 'assigned':
      return d.assignee ? `assigned to ${d.assignee_name ?? `${d.assignee.type} ${d.assignee.id}`}` : 'unassigned'
    case 'merged':
      return `merged ${groups} into this group${moved}`
    case 'unmerged':
      return `unmerged from ${groups}${moved}`
    case 'split':
      return `split with ${groups}${moved}`
    default:
      return entry.kind
  }
}

interface GroupOptions {
  projectId: string
  groupId: string
  json?: boolean
}

interface CommentOptions extends GroupOptions {
  body?: string
  edit?: string
  delete?: string
}

interface ActivityOptions extends GroupOptions {
  page?: string
  pageSize?: string
}

function ids(options: GroupOptions): { project_id: number; group_id: number } | null {
  const project_id = parseInt(options.projectId, 10)
  const group_id = parseInt(options.groupId, 10)
  if (isNaN(project_id) || isNaN(group_id)) {
    warning('Invalid project or group ID')
    return null
  }
  return { project_id, group_id }
}

export function registerActivityCommands(errors: Command): void {
  errors
    .command('comment')
    .description('Comment on an error group, or edit or delete a comment')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--group-id <id>', 'Error group ID')
    .option('--body <text>', 'The comment (at most 10,000 characters)')
    .option('--edit <comment-id>', 'Replace the text of your comment with --body')
    .option('--delete <comment-id>', 'Delete a comment (yours, or any as an administrator)')
    .option('--json', 'Output in JSON format')
    .action(commentAction)

  errors
    .command('activity')
    .description('Show the comments and history of an error group, newest first')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--group-id <id>', 'Error group ID')
    .option('--page <n>', 'Page number', '1')
    .option('--page-size <n>', 'Entries per page (max 100)', '20')
    .option('--json', 'Output in JSON format')
    .action(activityAction)
}

async function commentAction(options: CommentOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const path = ids(options)
  if (!path) return

  if (options.delete) {
    const comment_id = parseInt(options.delete, 10)
    if (isNaN(comment_id)) {
      warning('Invalid comment ID')
      return
    }
    await withSpinner('Deleting comment...', async () => {
      const { error } = await deleteErrorGroupComment({ client, path: { ...path, comment_id } })
      if (error) throw new Error(getErrorMessage(error))
    })
    success(`Comment #${comment_id} deleted`)
    return
  }

  let body: string
  try {
    body = checkCommentBody(options.body)
  } catch (e) {
    warning(e instanceof Error ? e.message : String(e))
    return
  }

  if (options.edit) {
    const comment_id = parseInt(options.edit, 10)
    if (isNaN(comment_id)) {
      warning('Invalid comment ID')
      return
    }
    const result = await withSpinner('Changing comment...', async () => {
      const { data, error } = await updateErrorGroupComment({ client, path: { ...path, comment_id }, body: { body } })
      if (error || !data) throw new Error(getErrorMessage(error))
      return data
    })
    if (options.json) {
      json(result)
      return
    }
    success(`Comment #${result.id} changed`)
    return
  }

  const result = await withSpinner('Adding comment...', async () => {
    const { data, error } = await createErrorGroupComment({ client, path, body: { body } })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(`Comment #${result.id} added to error group #${path.group_id}`)
}

async function activityAction(options: ActivityOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const path = ids(options)
  if (!path) return
  const page = Math.max(1, parseInt(options.page ?? '1', 10) || 1)
  const page_size = Math.min(100, Math.max(1, parseInt(options.pageSize ?? '20', 10) || 20))

  const result = await withSpinner('Loading activity...', async () => {
    const { data, error } = await listErrorGroupActivity({ client, path, query: { page, page_size } })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  header(`Activity of error group #${path.group_id}`)
  if (result.data.length === 0) {
    info('Nothing yet. Add a comment with: temps errors comment --project-id <id> --group-id <id> --body "..."')
    return
  }
  printTable(result.data, [
    { header: 'ID', accessor: (row) => String(row.id) },
    { header: 'When', accessor: (row) => formatRelativeTime(row.created_at) },
    { header: 'Who', accessor: (row) => (row.kind === 'regressed' ? '(event)' : (row.actor_name ?? 'someone')) },
    { header: 'What', accessor: (row) => (row.kind === 'comment' ? 'comment' : row.kind.replace(/_/g, ' ')) },
    { header: 'Details', accessor: (row) => describeActivityEntry(row) },
  ])
  const pages = Math.max(1, Math.ceil(result.total_count / result.page_size))
  if (pages > 1) {
    info(`Page ${result.page} of ${pages}; older entries with --page ${Math.min(pages, result.page + 1)}`)
  }
}
