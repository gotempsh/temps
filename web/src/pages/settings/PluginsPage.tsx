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
import {
  usePlugins,
  usePluginAvailability,
  usePluginStatus,
  useInstallPlugin,
  useReloadPlugins,
  useCanManagePlugins,
  usePluginCatalog,
} from '@/hooks/usePlugins'
import type { PluginCatalogEntry } from '@/hooks/usePlugins'
import { problemMessage } from '@/components/settings/oidc-provider-constants'
import {
  AlertCircle,
  CheckCircle2,
  Copy,
  ExternalLink,
  Loader2,
  Puzzle,
  RefreshCw,
  Sparkles,
} from 'lucide-react'
import { useEffect } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Skeleton } from '@/components/ui/skeleton'

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
      <VibeTempsInstallCard />
      <PluginCatalogCard />

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

/**
 * Everything published in the plugin registry, whether or not this build can
 * install it.
 *
 * Rendered unconditionally, including when the registry is unreachable: an
 * operator on an air-gapped box needs to be told *that the catalogue exists
 * and which host it could not reach*, not shown an empty page they will read
 * as "there are no plugins". Same reason entries this release cannot install
 * are listed rather than filtered — "upgrade temps to get this" is
 * actionable; silence is not.
 */
function PluginCatalogCard() {
  const canManagePlugins = useCanManagePlugins()
  const catalogQuery = usePluginCatalog({ enabled: canManagePlugins })

  return (
    <Card>
      <CardHeader>
        <CardTitle>Available plugins</CardTitle>
        <CardDescription>
          Plugins published in the Temps registry. Each installs as a single
          binary on this server — nothing is hosted for you.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {!canManagePlugins ? (
          <p className="text-sm text-muted-foreground">
            Browsing and installing plugins requires an administrator account.
          </p>
        ) : catalogQuery.isLoading ? (
          <div className="space-y-3">
            <Skeleton className="h-24 w-full" />
          </div>
        ) : catalogQuery.isError ? (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Could not load the plugin catalogue</AlertTitle>
            <AlertDescription className="space-y-2">
              <p>{problemMessage(catalogQuery.error, 'Unknown error')}</p>
              <Button
                variant="outline"
                size="sm"
                onClick={() => catalogQuery.refetch()}
              >
                Retry
              </Button>
            </AlertDescription>
          </Alert>
        ) : !catalogQuery.data?.available ? (
          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Registry unreachable</AlertTitle>
            <AlertDescription className="space-y-2">
              <p>
                {catalogQuery.data?.reason ??
                  'The plugin registry could not be reached.'}
              </p>
              <p className="text-xs text-muted-foreground">
                This instance browses plugins from {catalogQuery.data?.source}.
                Plugins already installed here keep running — only the list of
                what is available is unavailable.
              </p>
              <Button
                variant="outline"
                size="sm"
                onClick={() => catalogQuery.refetch()}
              >
                Retry
              </Button>
            </AlertDescription>
          </Alert>
        ) : catalogQuery.data.plugins.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            The registry has no published plugins yet.
          </p>
        ) : (
          catalogQuery.data.plugins.map((entry) => (
            <PluginCatalogRow key={entry.name} entry={entry} />
          ))
        )}
      </CardContent>
    </Card>
  )
}

/** One catalogue entry, with its install control or its refusal reason. */
function PluginCatalogRow({ entry }: { entry: PluginCatalogEntry }) {
  const installMutation = useInstallPlugin(entry.name)

  const handleInstall = () => {
    installMutation.mutate(undefined, {
      onSuccess: (result) => toast.success(result.message),
      onError: (error) =>
        toast.error(problemMessage(error, `Failed to install ${entry.title}`)),
    })
  }

  return (
    <div className="rounded-lg border p-4 space-y-3">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <p className="text-sm font-medium">{entry.title}</p>
            {entry.latest_version ? (
              <Badge variant="secondary" className="text-xs">
                v{entry.latest_version}
              </Badge>
            ) : (
              <Badge variant="outline" className="text-xs">
                No release yet
              </Badge>
            )}
            <Badge variant="outline" className="text-xs">
              {entry.category}
            </Badge>
            {entry.installed && (
              <Badge
                variant="default"
                className="bg-green-500/15 text-green-700 dark:text-green-400 border-green-500/20 text-xs"
              >
                Installed
              </Badge>
            )}
          </div>
          <p className="text-xs text-muted-foreground mt-1">{entry.summary}</p>
          <p className="text-xs text-muted-foreground mt-0.5">
            by {entry.author}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {entry.docs_url && (
            <Button asChild variant="ghost" size="sm">
              <a href={entry.docs_url} target="_blank" rel="noopener noreferrer">
                Docs
                <ExternalLink className="ml-1 h-3 w-3" />
              </a>
            </Button>
          )}
          {entry.installable && !entry.installed && (
            <Button
              size="sm"
              onClick={handleInstall}
              disabled={installMutation.isPending || !entry.latest_version}
            >
              {installMutation.isPending ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Installing…
                </>
              ) : (
                'Install'
              )}
            </Button>
          )}
        </div>
      </div>

      {/* Local verification refused this entry. The distinction matters to the
          operator: one is "you are behind", the other is "do not trust what
          the registry just told this instance". */}
      {!entry.installable && entry.reason && (
        <Alert
          variant={
            entry.rejection === 'manifest_url_mismatch' ? 'destructive' : 'default'
          }
        >
          <AlertCircle className="h-4 w-4" />
          <AlertTitle>
            {entry.rejection === 'manifest_url_mismatch'
              ? 'Registry mismatch — not installable'
              : 'Not installable on this version'}
          </AlertTitle>
          <AlertDescription>{entry.reason}</AlertDescription>
        </Alert>
      )}

      {installMutation.isError && (
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertTitle>Install failed</AlertTitle>
          <AlertDescription>
            {problemMessage(installMutation.error, 'Unknown error')}
          </AlertDescription>
        </Alert>
      )}
    </div>
  )
}

const VIBETEMPS_PLUGIN_NAME = 'vibetemps'

/**
 * Install card for VibeTemps — the one plugin the platform knows how to
 * fetch and install for you (everything else is the manual binary-drop flow
 * below). Always rendered, regardless of whether VibeTemps is configured:
 * an unconfigured feature must onboard the operator, not disappear.
 */
function VibeTempsInstallCard() {
  const canManagePlugins = useCanManagePlugins()
  const statusQuery = usePluginStatus(VIBETEMPS_PLUGIN_NAME)
  const availabilityQuery = usePluginAvailability(VIBETEMPS_PLUGIN_NAME, {
    enabled: canManagePlugins,
  })
  const installMutation = useInstallPlugin(VIBETEMPS_PLUGIN_NAME)

  const configured = statusQuery.data?.configured ?? false
  const reason = statusQuery.data?.reason
  const setupPath = statusQuery.data?.setup_path
  const manifest = availabilityQuery.data?.manifest

  const handleInstall = () => {
    // No version argument: the manifest URL always resolves to the current
    // release, so `version` on the request is a pin the server cannot honour
    // and rejects outright. Echoing the version we happen to have just read
    // would fail every install with "Version Pinning Not Supported".
    installMutation.mutate(undefined, {
      onSuccess: (result) => {
        toast.success(result.message)
      },
      onError: (error) => {
        toast.error(problemMessage(error, 'Failed to install VibeTemps'))
      },
    })
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
          <div className="flex items-start gap-3">
            <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-muted">
              <Sparkles className="h-4 w-4" />
            </div>
            <div>
              <div className="flex items-center gap-2">
                <CardTitle>VibeTemps</CardTitle>
                {manifest?.version && (
                  <Badge variant="secondary" className="text-xs">
                    v{manifest.version}
                  </Badge>
                )}
              </div>
              <CardDescription className="mt-1">
                An AI app builder embedded in the Temps platform — describe
                what you want and it scaffolds, previews, and deploys the app
                for you, right inside the console.
              </CardDescription>
            </div>
          </div>
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        {statusQuery.isLoading ? (
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            Checking VibeTemps status…
          </div>
        ) : statusQuery.isError ? (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Could not check VibeTemps status</AlertTitle>
            <AlertDescription>
              {problemMessage(statusQuery.error, 'Unknown error')}
            </AlertDescription>
          </Alert>
        ) : configured ? (
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex items-center gap-2 text-sm">
              <CheckCircle2 className="h-4 w-4 text-green-600 dark:text-green-400" />
              <span>VibeTemps is installed and running.</span>
            </div>
            {setupPath && (
              <Button asChild variant="outline" size="sm">
                <Link to={setupPath}>Open VibeTemps</Link>
              </Button>
            )}
          </div>
        ) : (
          <div className="space-y-3">
            <div className="rounded-lg border border-dashed bg-muted/30 p-3">
              <p className="text-sm">
                {reason ?? 'VibeTemps is not installed on this instance.'}
              </p>
            </div>
            {installMutation.isError && (
              <Alert variant="destructive">
                <AlertCircle className="h-4 w-4" />
                <AlertTitle>Install failed</AlertTitle>
                <AlertDescription>
                  {problemMessage(installMutation.error, 'Unknown error')}
                </AlertDescription>
              </Alert>
            )}
            {canManagePlugins ? (
              <div className="flex items-center gap-2">
                <Button
                  onClick={handleInstall}
                  disabled={installMutation.isPending}
                >
                  {installMutation.isPending ? (
                    <>
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                      Installing…
                    </>
                  ) : (
                    'Install VibeTemps'
                  )}
                </Button>
                {availabilityQuery.data?.reason && (
                  <span className="text-xs text-muted-foreground">
                    {availabilityQuery.data.reason}
                  </span>
                )}
              </div>
            ) : (
              <p className="text-xs text-muted-foreground">
                Only platform administrators can install plugins. Ask an
                admin to install VibeTemps from this page.
              </p>
            )}
          </div>
        )}
      </CardContent>
    </Card>
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
