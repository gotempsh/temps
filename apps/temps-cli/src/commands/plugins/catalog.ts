import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import type { ProblemDetails } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, info, warning, error as printError } from '../../ui/output.js'

// ============================================================================
// Hand-written request/response shapes
// ============================================================================
//
// `GET /x/plugins/catalog` is a core route owned by
// crates/temps-external-plugins/src/handler.rs, but the committed
// openapi.json has not been regenerated to include it yet — same situation
// as the registry/status routes in ./list.ts, and the same fallback: mirror
// the handler's serde structs here and call the shared `client` object's
// generic methods. Delete these once the generated client gains the types.

/** Why a catalogued plugin cannot be installed by the connected instance. */
export type CatalogRejection = 'unknown_to_this_release' | 'manifest_url_mismatch'

export interface PluginCatalogEntry {
  name: string
  title: string
  summary: string
  description: string
  author: string
  category: string
  repository?: string | null
  docs_url?: string | null
  latest_version?: string | null
  platforms: string[]
  installed: boolean
  installable: boolean
  rejection?: CatalogRejection | null
  reason?: string | null
}

export interface PluginCatalogResponse {
  available: boolean
  reason?: string | null
  source: string
  plugins: PluginCatalogEntry[]
}

interface CatalogRow {
  name: string
  version: string
  category: string
  status: string
}

/**
 * `temps plugins catalog` — everything published in the registry.
 *
 * Distinct from `temps plugins list`, which reports only what the connected
 * instance already knows how to install. A plugin released after that
 * instance's binary was built can appear here and nowhere else, which is the
 * whole point: without it, "we published a plugin" is invisible to anyone who
 * has not upgraded, and they have no way to find out that upgrading is what
 * they need to do.
 */
export async function pluginCatalogAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const catalog = await withSpinner('Fetching the plugin catalogue...', async () => {
    const { data, error, response } = await client.get<PluginCatalogResponse, ProblemDetails>({
      url: '/x/plugins/catalog',
    })
    if (error || !data) {
      throw new Error(
        getErrorMessage(error) ||
          (response ? `Request failed with status ${response.status}` : 'Unknown error'),
      )
    }
    return data
  })

  if (options.json) {
    json(catalog)
    return
  }

  newline()
  header(`${icons.info} Plugin Catalogue`)

  // An unreachable registry is a reportable state, not an empty list: printing
  // "no plugins" here would be a false statement rather than a neutral one.
  if (!catalog.available) {
    printError(catalog.reason ?? 'The plugin registry could not be reached')
    info(`Registry: ${catalog.source}`)
    info('Plugins already installed on this instance keep running.')
    newline()
    return
  }

  if (catalog.plugins.length === 0) {
    warning('The registry has no published plugins yet')
    newline()
    return
  }

  const rows: CatalogRow[] = catalog.plugins.map((entry) => ({
    name: entry.name,
    version: entry.latest_version ?? '-',
    category: entry.category,
    status: entry.installed ? 'installed' : entry.installable ? 'installable' : 'unavailable',
  }))

  const columns: TableColumn<CatalogRow>[] = [
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Version', key: 'version' },
    { header: 'Category', key: 'category' },
    {
      header: 'Status',
      key: 'status',
      color: (v) =>
        v === 'installed' ? colors.success(v) : v === 'installable' ? v : colors.muted(v),
    },
  ]

  printTable(rows, columns, { style: 'minimal' })

  for (const entry of catalog.plugins) {
    if (!entry.installable && entry.reason) {
      warning(`${entry.name}: ${entry.reason}`)
    } else if (!entry.installed) {
      info(`Run: temps plugins install ${entry.name}`)
    }
  }
  newline()
}
