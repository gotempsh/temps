import { requireAuth } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, formatRelativeTime } from '../../ui/output.js'
import { getClient } from '../../api/client.js'

interface Project {
  id: number
  name: string
  description?: string
  status?: string
  created_at: string
  updated_at: string
}

interface ListOptions {
  json?: boolean
}

export async function list(options: ListOptions): Promise<void> {
  await requireAuth()

  const client = getClient()

  const projects = await withSpinner('Fetching projects...', async () => {
    const response = await client.get('/api/projects')

    if (response.error) {
      throw new Error('Failed to fetch projects')
    }

    return (response.data ?? []) as Project[]
  })

  if (options.json) {
    json(projects)
    return
  }

  newline()
  header(`${icons.folder} Projects (${projects.length})`)

  const columns: TableColumn<Project>[] = [
    { header: 'ID', key: 'id', width: 8 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Description', accessor: (p) => p.description ?? '-', width: 30 },
    {
      header: 'Status',
      accessor: (p) => p.status ?? 'active',
      color: (v) => statusBadge(v),
    },
    {
      header: 'Updated',
      accessor: (p) => formatRelativeTime(p.updated_at),
      color: (v) => colors.muted(v),
    },
  ]

  printTable(projects, columns, { style: 'minimal' })
  newline()
}
