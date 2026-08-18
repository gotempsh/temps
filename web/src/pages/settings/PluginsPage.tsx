import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { usePlugins, useReloadPlugins } from '@/hooks/usePlugins'
import {
  AlertCircle,
  Copy,
  ExternalLink,
  Loader2,
  Puzzle,
  RefreshCw,
} from 'lucide-react'
import { useEffect } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'

export function PluginsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: plugins = [], isLoading, error } = usePlugins()
  const reloadPlugins = useReloadPlugins()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Plugins' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Plugins')

  const handleReload = async () => {
    try {
      const result = await reloadPlugins.mutateAsync()
      toast.success(result.message)
    } catch {
      toast.error('Failed to reload plugins')
    }
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <Loader2 className="h-8 w-8 animate-spin" />
      </div>
    )
  }

  if (error) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Error</AlertTitle>
        <AlertDescription>Failed to load plugins.</AlertDescription>
      </Alert>
    )
  }

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
            <div>
              <CardTitle>External Plugins</CardTitle>
              <CardDescription>
                Manage external plugin binaries. Plugins are discovered from the
                plugins directory on startup or reload.
              </CardDescription>
            </div>
            <Button
              variant="outline"
              onClick={handleReload}
              disabled={reloadPlugins.isPending}
            >
              {reloadPlugins.isPending ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : (
                <RefreshCw className="mr-2 h-4 w-4" />
              )}
              <span className="hidden sm:inline">Reload Plugins</span>
              <span className="sm:hidden">Reload</span>
            </Button>
          </div>
        </CardHeader>
        <CardContent className="space-y-4">
          <PluginSetupHelp />
          {plugins.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12 text-center">
              <Puzzle className="h-12 w-12 text-muted-foreground mb-4" />
              <p className="text-sm font-medium">No plugins installed</p>
              <p className="text-sm text-muted-foreground mt-1">
                Place plugin binaries in the plugins directory and click Reload.
              </p>
            </div>
          ) : (
            <div className="space-y-3">
              {plugins.map((plugin) => (
                <div
                  key={plugin.name}
                  className="flex items-center justify-between rounded-lg border p-4"
                >
                  <div className="flex items-center gap-3 min-w-0">
                    <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-muted">
                      <Puzzle className="h-4 w-4" />
                    </div>
                    <div className="min-w-0">
                      <div className="flex items-center gap-2">
                        <p className="text-sm font-medium truncate">
                          {plugin.display_name || plugin.name}
                        </p>
                        <Badge variant="secondary" className="text-xs shrink-0">
                          v{plugin.version}
                        </Badge>
                      </div>
                      {plugin.description && (
                        <p className="text-xs text-muted-foreground truncate mt-0.5">
                          {plugin.description}
                        </p>
                      )}
                    </div>
                  </div>
                  <div className="flex items-center gap-2 shrink-0 ml-4">
                    {plugin.ui && (
                      <Badge variant="outline" className="text-xs">
                        UI
                      </Badge>
                    )}
                    {plugin.requires_db && (
                      <Badge variant="outline" className="text-xs">
                        DB
                      </Badge>
                    )}
                    <Badge
                      variant="default"
                      className="bg-green-500/15 text-green-700 dark:text-green-400 border-green-500/20 text-xs"
                    >
                      Running
                    </Badge>
                    {/* A plugin listed here with no way to reach it sends the
                        user hunting through the sidebar.

                        Gated on the nav entry, not on `plugin.ui`: that field
                        describes a *declared* bundle, and a plugin can serve
                        its UI from `/ui/` without one (some plugins do, and
                        reports `ui: null`). What actually makes a plugin
                        reachable is a platform/settings nav entry — those are
                        what `/plugins/:pluginName` routes to. Project-scoped
                        entries live under a project and have no address from
                        here. */}
                    {plugin.nav.some((e) => e.section !== 'project') && (
                      <Button asChild variant="outline" size="sm">
                        <Link to={`/plugins/${plugin.name}`}>Open</Link>
                      </Button>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>

      <PluginExamples />
    </div>
  )
}

const PLUGINS_REPO_URL = 'https://github.com/gotempsh/plugins'

const EXAMPLE_PLUGINS: Array<{
  name: string
  description: string
  path: string
}> = [
  {
    name: 'example-plugin',
    description:
      'Minimal "hello world" plugin — the shortest path to understanding the plugin protocol and UI bundle layout.',
    path: 'example-plugin',
  },
  {
    name: 'lighthouse-plugin',
    description:
      'Runs Lighthouse audits after deployments and tracks Core Web Vitals over time.',
    path: 'lighthouse-plugin',
  },
  {
    name: 'indexnow-plugin',
    description:
      'Automatically submits deployed URLs to Bing, Yandex, and other IndexNow-supporting search engines.',
    path: 'indexnow-plugin',
  },
  {
    name: 'google-indexing-plugin',
    description:
      'Notifies the Google Indexing API when pages are published or removed.',
    path: 'google-indexing-plugin',
  },
]

function PluginExamples() {
  return (
    <Card>
      <CardHeader>
        <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
          <div>
            <CardTitle>Example Plugins</CardTitle>
            <CardDescription>
              Official plugins maintained in{' '}
              <a
                href={PLUGINS_REPO_URL}
                target="_blank"
                rel="noopener noreferrer"
                className="font-medium text-primary hover:underline"
              >
                gotempsh/plugins
              </a>
              . Clone the repo, run <code>cargo build --release</code>, and
              copy the binary into your plugins directory.
            </CardDescription>
          </div>
          <a
            href={`${PLUGINS_REPO_URL}/releases/latest`}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1 text-xs font-medium text-primary hover:underline"
          >
            Prebuilt binaries
            <ExternalLink className="h-3 w-3" />
          </a>
        </div>
      </CardHeader>
      <CardContent>
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
          {EXAMPLE_PLUGINS.map((plugin) => (
            <a
              key={plugin.name}
              href={`${PLUGINS_REPO_URL}/tree/main/${plugin.path}`}
              target="_blank"
              rel="noopener noreferrer"
              className="group rounded-lg border p-4 transition-colors hover:bg-accent"
            >
              <div className="flex items-start justify-between gap-2">
                <div className="flex items-center gap-2 min-w-0">
                  <Puzzle className="h-4 w-4 shrink-0 text-muted-foreground" />
                  <p className="text-sm font-medium truncate">{plugin.name}</p>
                </div>
                <ExternalLink className="h-3 w-3 shrink-0 text-muted-foreground transition-colors group-hover:text-foreground" />
              </div>
              <p className="mt-2 text-xs text-muted-foreground">
                {plugin.description}
              </p>
            </a>
          ))}
        </div>
      </CardContent>
    </Card>
  )
}

function PluginSetupHelp() {
  const pluginsDir = '~/.temps/plugins'

  const handleCopy = (value: string) => {
    navigator.clipboard.writeText(value)
    toast.success('Copied to clipboard')
  }

  return (
    <div className="rounded-lg border border-dashed bg-muted/30 p-4">
      <div className="flex items-start gap-3">
        <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-background">
          <Puzzle className="h-4 w-4 text-muted-foreground" />
        </div>
        <div className="min-w-0 flex-1 space-y-3">
          <div>
            <p className="text-sm font-medium">How to install a plugin</p>
            <p className="text-xs text-muted-foreground mt-0.5">
              Temps loads executable binaries from the plugins directory over
              stdin/stdout. Drop a binary in, click Reload, and it shows up
              below.
            </p>
          </div>

          <ol className="space-y-2 text-xs text-muted-foreground">
            <li className="flex gap-2">
              <span className="font-medium text-foreground">1.</span>
              <div className="flex-1 min-w-0">
                <p>
                  Place the plugin binary in the plugins directory (override
                  with <code>TEMPS_DATA_DIR</code>):
                </p>
                <div className="mt-1 flex items-center gap-2 rounded-md bg-background px-3 py-2 font-mono text-xs">
                  <span className="flex-1 overflow-x-auto">{pluginsDir}</span>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-6 w-6 shrink-0"
                    onClick={() => handleCopy(pluginsDir)}
                  >
                    <Copy className="h-3 w-3" />
                  </Button>
                </div>
              </div>
            </li>
            <li className="flex gap-2">
              <span className="font-medium text-foreground">2.</span>
              <p className="flex-1">
                Ensure the file is executable (
                <code>chmod +x ./my-plugin</code>).
              </p>
            </li>
            <li className="flex gap-2">
              <span className="font-medium text-foreground">3.</span>
              <p className="flex-1">
                Click <span className="font-medium">Reload Plugins</span> above
                to discover and start it.
              </p>
            </li>
          </ol>

          <a
            href="https://temps.sh/docs/plugins"
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1 text-xs font-medium text-primary hover:underline"
          >
            Read the plugin system docs
            <ExternalLink className="h-3 w-3" />
          </a>
        </div>
      </div>
    </div>
  )
}
