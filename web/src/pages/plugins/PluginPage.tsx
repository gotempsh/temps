import { usePluginsContext } from '@/contexts/PluginsContext'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import { Loader2 } from 'lucide-react'
import { useCallback, useRef, useState } from 'react'
import { useParams } from 'react-router'

/**
 * Renders an external plugin's UI inside an iframe.
 *
 * The plugin serves its own HTML/JS/CSS at `/api/x/{pluginName}/ui/`.
 * This component wraps it in a full-height iframe that communicates
 * via postMessage for theme and navigation events.
 */
export function PluginPage() {
  const { pluginName } = useParams<{ pluginName: string }>()
  const { getPlugin } = usePluginsContext()
  const iframeRef = useRef<HTMLIFrameElement>(null)
  const [isLoading, setIsLoading] = useState(true)
  const [hasError, setHasError] = useState(false)

  const plugin = pluginName ? getPlugin(pluginName) : undefined

  const handleLoad = useCallback(() => {
    setIsLoading(false)
  }, [])

  const handleError = useCallback(() => {
    setIsLoading(false)
    setHasError(true)
  }, [])

  if (!plugin) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[400px] text-muted-foreground">
        <p>Plugin not found: {pluginName}</p>
      </div>
    )
  }

  const Icon = resolvePluginIcon(plugin.nav[0]?.icon ?? 'puzzle')
  const displayName = plugin.display_name ?? plugin.name

  // The plugin UI is served by the plugin itself, proxied through the standard
  // API proxy at /api/x/{name}/. This works in both dev (rsbuild proxies /api)
  // and production (SPA fallback skips /api/ paths).
  const iframeSrc = `/api/x/${plugin.name}/ui/`

  return (
    <div className="flex flex-col h-[calc(100vh-4rem)]">
      {/* Compact header */}
      <div className="flex items-center gap-2 px-4 py-2 border-b bg-background shrink-0">
        <div className="flex h-7 w-7 items-center justify-center rounded-md bg-muted">
          <Icon className="h-4 w-4" />
        </div>
        <h1 className="text-sm font-medium">{displayName}</h1>
        <span className="text-xs text-muted-foreground font-mono">
          v{plugin.version}
        </span>
        {isLoading && (
          <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground ml-auto" />
        )}
      </div>

      {/* iframe container */}
      {hasError ? (
        <div className="flex flex-col items-center justify-center flex-1 text-muted-foreground gap-2">
          <p className="text-sm">
            Failed to load plugin UI from{' '}
            <code className="rounded bg-muted px-1.5 py-0.5 text-xs font-mono">
              {iframeSrc}
            </code>
          </p>
          <p className="text-xs">
            Make sure the plugin serves HTML at its <code>/ui/</code> endpoint.
          </p>
        </div>
      ) : (
        <iframe
          ref={iframeRef}
          src={iframeSrc}
          title={`${displayName} plugin`}
          className="flex-1 w-full border-0"
          onLoad={handleLoad}
          onError={handleError}
          sandbox="allow-scripts allow-forms allow-same-origin allow-popups"
        />
      )}
    </div>
  )
}
