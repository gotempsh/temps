import { requireAuth } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, formatRelativeTime } from '../../ui/output.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getProjects } from '../../api/sdk.gen.js'
import type { ProjectResponse } from '../../api/types.gen.js'

interface ListOptions {
  json?: boolean
}

export async function list(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projects = await withSpinner('Fetching projects...', async () => {
    const { data, error } = await getProjects({ client })

    if (error) {
      throw new Error(getErrorMessage(error))
    }

    return data?.projects ?? []
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
      accessor: (p) => formatRelativeTime(new Date(p.updated_at * 1000).toISOString()),
      color: (v) => colors.muted(v),
    },
  ]

  printTable(projects, columns, { style: 'minimal' })
  newline()
}
