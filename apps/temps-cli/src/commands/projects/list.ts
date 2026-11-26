// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, formatRelativeTime } from '../../ui/output.js'
import { setupClient, client, getErrorMessage, requireApiShape } from '../../lib/api-client.js'
import { getProjects } from '../../api/sdk.gen.js'
import type { ProjectResponse } from '../../api/types.gen.js'
import { parseIntegerFlag } from '../../lib/command-kit.js'
import { ValidationError } from '../../utils/errors.js'

interface ListOptions {
  json?: boolean
  page?: string
  perPage?: string
  group?: string
  ungrouped?: boolean
}

export async function list(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const page = options.page ? parseInt(options.page, 10) : undefined
  const perPage = options.perPage ? parseInt(options.perPage, 10) : undefined
  if (options.group !== undefined && options.ungrouped) {
    throw new ValidationError('--group and --ungrouped cannot be combined')
  }
  const groupId = options.group === undefined ? undefined : parseIntegerFlag(options.group, '--group', 1)

  const projects = await withSpinner('Fetching projects...', async () => {
    const { data, error } = await getProjects({
      client,
      query: {
        ...(page && { page }),
        ...(perPage && { per_page: perPage }),
        ...(groupId !== undefined && { project_group_id: groupId }),
        ...(options.ungrouped && { ungrouped: true }),
      },
    })

    if (error) {
      throw new Error(getErrorMessage(error))
    }

    const body = requireApiShape(
      data,
      'listing projects',
      (value): value is { projects: ProjectResponse[] } =>
        typeof value === 'object' &&
        value !== null &&
        'projects' in value &&
        Array.isArray(value.projects),
    )
    return body.projects
  })

  if (options.json) {
    json(projects)
    return
  }

  newline()
  header(`${icons.folder} Projects (${projects.length})`)
  const columns: TableColumn<ProjectResponse>[] = [
    { header: 'ID', key: 'id', width: 8 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(String(v)) },
    { header: 'Slug', key: 'slug', width: 20 },
    { header: 'Branch', key: 'main_branch', width: 15 },
    {
      header: 'Updated',
      accessor: (p) => formatRelativeTime(p.updated_at),
      color: (v) => colors.muted(v),
    },
  ]

  printTable(projects, columns, { style: 'minimal' })
  newline()
}
