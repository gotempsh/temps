import { usePluginsContext } from '@/contexts/PluginsContext'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import { useParams } from 'react-router-dom'

/**
 * Generic page rendered for external plugin routes.
 *
 * For plugins that provide a UI bundle (future), this component will
 * load and mount the plugin's JS/CSS assets. For API-only plugins,
 * it shows the plugin info and a link to explore the API.
 */
export function PluginPage() {
  const { pluginName } = useParams<{ pluginName: string }>()
  const { getPlugin } = usePluginsContext()

  const plugin = pluginName ? getPlugin(pluginName) : undefined

  if (!plugin) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[400px] text-muted-foreground">
        <p>Plugin not found: {pluginName}</p>
      </div>
    )
  }

  const Icon = resolvePluginIcon(plugin.nav[0]?.icon ?? 'puzzle')
  const displayName = plugin.display_name ?? plugin.name

  return (
    <div className="max-w-4xl mx-auto p-6">
      <div className="flex items-center gap-3 mb-6">
        <div className="flex h-10 w-10 items-center justify-center rounded-lg bg-muted">
          <Icon className="h-5 w-5" />
        </div>
        <div>
          <h1 className="text-2xl font-semibold">{displayName}</h1>
          {plugin.description && (
            <p className="text-sm text-muted-foreground">
              {plugin.description}
            </p>
          )}
        </div>
        <span className="ml-auto text-xs text-muted-foreground font-mono">
          v{plugin.version}
        </span>
      </div>

      <div className="rounded-lg border p-6">
        <h2 className="text-lg font-medium mb-4">Plugin API</h2>
        <p className="text-sm text-muted-foreground mb-4">
          This plugin exposes an API at{' '}
          <code className="rounded bg-muted px-1.5 py-0.5 text-xs font-mono">
            /api/x/{plugin.name}/
          </code>
        </p>
        <div className="grid grid-cols-2 gap-4 text-sm">
          <div>
            <span className="text-muted-foreground">Name:</span>{' '}
            <span className="font-mono">{plugin.name}</span>
          </div>
          <div>
            <span className="text-muted-foreground">Version:</span>{' '}
            <span className="font-mono">{plugin.version}</span>
          </div>
          <div>
            <span className="text-muted-foreground">Requires DB:</span>{' '}
            <span>{plugin.requires_db ? 'Yes' : 'No'}</span>
          </div>
          <div>
            <span className="text-muted-foreground">Health check:</span>{' '}
            <code className="rounded bg-muted px-1.5 py-0.5 text-xs font-mono">
              /api/x/{plugin.name}{plugin.health_path}
            </code>
          </div>
        </div>
      </div>
    </div>
  )
}
