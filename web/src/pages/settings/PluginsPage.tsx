// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  RegistryPlugin,
  ReloadFailureResponse,
} from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogCancel,
} from '@/components/ui/alert-dialog'
import { useAuth } from '@/contexts/AuthContext'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  useInstallPlugin,
  usePluginCatalog,
  usePlugins,
  useReloadPlugins,
  useUninstallPlugin,
} from '@/hooks/usePlugins'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import {
  canManageExternalPlugins,
  pluginInstallAction,
  pluginReloadFailures,
  safeRegistryNavigationUrl,
} from '@/lib/plugin-registry'
import { sensitiveActionErrorMessage } from '@/lib/sensitiveActionProblem'
import {
  AlertCircle,
  ExternalLink,
  Loader2,
  Puzzle,
  RefreshCw,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'

export function PluginsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { user } = useAuth()
  const canManagePlugins = canManageExternalPlugins(user?.role)
  const { data: plugins = [], isLoading: pluginsLoading } = usePlugins()
  const {
    data: catalog,
    isLoading: catalogLoading,
    error: catalogError,
  } = usePluginCatalog(canManagePlugins)
  const installPlugin = useInstallPlugin()
  const reloadPlugins = useReloadPlugins()
  const uninstallPlugin = useUninstallPlugin()
  const [uninstallName, setUninstallName] = useState<string | null>(null)
  const [reloadFailures, setReloadFailures] = useState<ReloadFailureResponse[]>(
    []
  )
  const managementPending =
    installPlugin.isPending ||
    reloadPlugins.isPending ||
    uninstallPlugin.isPending
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Plugins' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Plugins')

  const handleReload = async () => {
    setReloadFailures([])
    try {
      const result = await reloadPlugins.mutateAsync()
      setReloadFailures(pluginReloadFailures(result))
      if (result.failures.length > 0) {
        toast.warning(result.message)
      } else {
        toast.success(result.message)
      }
    } catch (error) {
      if (handleSensitiveActionError(error, () => void handleReload())) return
      setReloadFailures(pluginReloadFailures(error))
      toast.error(
        sensitiveActionErrorMessage(error, 'Failed to reload plugins.')
      )
    }
  }

  const handleInstall = async (name: string) => {
    try {
      const result = await installPlugin.mutateAsync(name)
      toast.success(result.message)
    } catch (error) {
      if (handleSensitiveActionError(error, () => void handleInstall(name))) {
        return
      }
      toast.error(
        sensitiveActionErrorMessage(error, `Failed to install ${name}.`)
      )
    }
  }

  const handleUninstall = async (name: string) => {
    try {
      const result = await uninstallPlugin.mutateAsync(name)
      setUninstallName(null)
      toast.success(result.message)
    } catch (error) {
      if (handleSensitiveActionError(error, () => void handleUninstall(name))) {
        setUninstallName(null)
        return
      }
      toast.error(
        sensitiveActionErrorMessage(error, `Failed to uninstall ${name}.`)
      )
    }
  }

  return (
    <div className="space-y-6">
      {canManagePlugins && verificationDialog}
      <AlertDialog
        open={canManagePlugins && uninstallName !== null}
        onOpenChange={(open) => {
          if (!open && !uninstallPlugin.isPending) setUninstallName(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Uninstall {uninstallName}?</AlertDialogTitle>
            <AlertDialogDescription>
              This stops the plugin and removes it from navigation. Its data is
              preserved so you can reinstall it later.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={uninstallPlugin.isPending}>
              Cancel
            </AlertDialogCancel>
            <Button
              variant="destructive"
              disabled={managementPending}
              onClick={() => {
                if (uninstallName) void handleUninstall(uninstallName)
              }}
            >
              {uninstallPlugin.isPending && (
                <Loader2 className="mr-2 size-4 animate-spin" />
              )}
              {uninstallPlugin.isPending ? 'Uninstalling…' : 'Uninstall'}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <Card>
        <CardHeader>
          <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
            <div>
              <CardTitle>External Plugins</CardTitle>
              <CardDescription>
                {canManagePlugins
                  ? 'Install signed, platform-specific releases from the trusted Temps registry.'
                  : 'View verified external plugins currently running in Temps.'}
              </CardDescription>
            </div>
            {canManagePlugins && (
              <Button
                variant="outline"
                onClick={handleReload}
                disabled={managementPending}
              >
                {reloadPlugins.isPending ? (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                ) : (
                  <RefreshCw className="mr-2 h-4 w-4" />
                )}
                <span className="hidden sm:inline">Reload Plugins</span>
                <span className="sm:hidden">Reload</span>
              </Button>
            )}
          </div>
        </CardHeader>
        <CardContent className="space-y-6">
          {canManagePlugins && reloadFailures.length > 0 && (
            <Alert variant="destructive">
              <AlertCircle className="size-4" />
              <AlertTitle>Some plugins could not be reloaded</AlertTitle>
              <AlertDescription>
                <ul className="list-disc space-y-1 pl-4">
                  {reloadFailures.map((failure, index) => (
                    <li
                      key={`${failure.plugin}-${index}`}
                      className="break-words"
                    >
                      <strong>{failure.plugin || 'Plugin registry'}:</strong>{' '}
                      {failure.reason}
                    </li>
                  ))}
                </ul>
              </AlertDescription>
            </Alert>
          )}
          {canManagePlugins && (
            <RegistryCatalog
              catalog={catalog}
              error={catalogError}
              isLoading={catalogLoading}
              installedVersions={
                new Map(plugins.map((plugin) => [plugin.name, plugin.version]))
              }
              installingName={
                installPlugin.isPending ? installPlugin.variables : undefined
              }
              onInstall={(name) => void handleInstall(name)}
              managementPending={managementPending}
            />
          )}

          <section
            className="space-y-3"
            aria-labelledby="running-plugins-title"
          >
            <div className="flex items-baseline justify-between gap-4 border-b pb-3">
              <div>
                <h2 id="running-plugins-title" className="font-semibold">
                  Running
                </h2>
                <p className="text-sm text-muted-foreground">
                  Verified plugins currently loaded by Temps.
                </p>
              </div>
              <span className="shrink-0 text-sm text-muted-foreground">
                {plugins.length} {plugins.length === 1 ? 'plugin' : 'plugins'}
              </span>
            </div>

            {pluginsLoading ? (
              <RunningPluginsSkeleton />
            ) : plugins.length === 0 ? (
              <div className="rounded-lg border border-dashed px-4 py-8 text-center">
                <Puzzle className="mx-auto size-5 text-muted-foreground" />
                <p className="mt-3 font-medium">
                  No verified plugins are running.
                </p>
                <p className="mt-1 text-sm text-muted-foreground">
                  {canManagePlugins
                    ? 'Install a registry release above to add one.'
                    : 'Ask a system administrator to install one.'}
                </p>
              </div>
            ) : (
              <div className="space-y-2">
                {plugins.map((plugin) => (
                  <div
                    key={plugin.name}
                    className="flex flex-col gap-3 rounded-lg border p-4 sm:flex-row sm:items-center sm:justify-between"
                  >
                    <div className="min-w-0">
                      <div className="flex flex-wrap items-center gap-2">
                        <p className="font-medium">
                          {plugin.display_name || plugin.name}
                        </p>
                        <Badge variant="secondary">v{plugin.version}</Badge>
                        <Badge className="border-green-500/20 bg-green-500/15 text-green-700 hover:bg-green-500/20 dark:text-green-400">
                          Running
                        </Badge>
                      </div>
                      {plugin.description && (
                        <p className="mt-1 text-sm text-muted-foreground">
                          {plugin.description}
                        </p>
                      )}
                    </div>
                    <div className="flex items-center gap-2">
                      {plugin.nav.some(
                        (entry) => entry.section !== 'project'
                      ) && (
                        <Button asChild variant="outline" size="sm">
                          <Link to={`/plugins/${plugin.name}`}>Open</Link>
                        </Button>
                      )}
                      {canManagePlugins && (
                        <Button
                          variant="outline"
                          size="sm"
                          disabled={managementPending}
                          onClick={() => setUninstallName(plugin.name)}
                        >
                          Uninstall
                        </Button>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </section>
        </CardContent>
      </Card>
    </div>
  )
}

interface RegistryCatalogProps {
  managementPending: boolean
  catalog?: {
    available: boolean
    plugins: RegistryPlugin[]
    reason?: string | null
    source: string
  }
  error: Error | null
  installedVersions: Map<string, string>
  installingName?: string
  isLoading: boolean
  onInstall: (name: string) => void
}

function RegistryCatalog({
  managementPending,
  catalog,
  error,
  installedVersions,
  installingName,
  isLoading,
  onInstall,
}: RegistryCatalogProps) {
  return (
    <section className="space-y-3" aria-labelledby="plugin-registry-title">
      <div className="flex flex-col gap-2 border-b pb-3 sm:flex-row sm:items-end sm:justify-between">
        <div>
          <h2 id="plugin-registry-title" className="font-semibold">
            Registry
          </h2>
          <p className="text-sm text-muted-foreground">
            Releases are selected for this server, hash-verified, and installed
            atomically.
          </p>
        </div>
        <a
          href="https://temps.sh/docs/plugins"
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-1 text-sm font-medium text-primary hover:underline"
        >
          Plugin documentation
          <ExternalLink className="size-3.5" />
        </a>
      </div>

      {isLoading ? (
        <CatalogSkeleton />
      ) : error ? (
        <Alert variant="destructive">
          <AlertCircle className="size-4" />
          <AlertTitle>Could not load the plugin registry</AlertTitle>
          <AlertDescription>
            {sensitiveActionErrorMessage(error, 'Try again in a moment.')}
          </AlertDescription>
        </Alert>
      ) : catalog?.available === false ? (
        <Alert>
          <AlertCircle className="size-4" />
          <AlertTitle>Plugin registry is not configured</AlertTitle>
          <AlertDescription>
            {catalog.reason ||
              'Configure the registry URL and trusted signing key, then restart Temps.'}
          </AlertDescription>
        </Alert>
      ) : catalog?.plugins.length === 0 ? (
        <div className="rounded-lg border border-dashed px-4 py-8 text-center">
          <Puzzle className="mx-auto size-5 text-muted-foreground" />
          <p className="mt-3 font-medium">The registry has no plugins yet.</p>
          <p className="mt-1 text-sm text-muted-foreground">
            Published releases will appear here automatically.
          </p>
        </div>
      ) : (
        <div className="@container">
          <div className="grid gap-3 @2xl:grid-cols-2">
            {catalog?.plugins.map((plugin) => (
              <RegistryPluginCard
                key={plugin.name}
                plugin={plugin}
                installedVersion={installedVersions.get(plugin.name)}
                installing={installingName === plugin.name}
                installDisabled={managementPending}
                onInstall={onInstall}
              />
            ))}
          </div>
        </div>
      )}
    </section>
  )
}

interface RegistryPluginCardProps {
  installDisabled: boolean
  installedVersion?: string
  installing: boolean
  onInstall: (name: string) => void
  plugin: RegistryPlugin
}

function RegistryPluginCard({
  installDisabled,
  installedVersion,
  installing,
  onInstall,
  plugin,
}: RegistryPluginCardProps) {
  const repositoryUrl = safeRegistryNavigationUrl(plugin.repository)
  const action = pluginInstallAction(installedVersion, plugin.version)
  const installed = action === 'installed'
  let actionLabel = action === 'upgrade' ? 'Upgrade' : 'Install'
  if (installed) actionLabel = 'Installed'
  if (installing)
    actionLabel = action === 'upgrade' ? 'Upgrading' : 'Installing'

  return (
    <article className="flex min-h-44 flex-col rounded-lg border p-4 transition-colors hover:bg-muted/30">
      <div className="flex min-w-0 items-start gap-3">
        <Puzzle className="mt-1 size-5 shrink-0 text-muted-foreground" />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="font-semibold">{plugin.title}</h3>
            <Badge variant="secondary">v{plugin.version}</Badge>
            <Badge variant="outline">{plugin.category}</Badge>
          </div>
          <p className="mt-1 font-mono text-sm text-muted-foreground">
            {plugin.name}
          </p>
        </div>
      </div>

      <p className="mt-3 flex-1 text-sm text-muted-foreground">
        {plugin.summary}
      </p>

      <div className="mt-4 flex flex-wrap items-center justify-between gap-3 border-t pt-3">
        <span className="text-sm text-muted-foreground">
          By {plugin.author}
        </span>
        <div className="flex items-center gap-2">
          {repositoryUrl && (
            <Button asChild variant="ghost" size="sm">
              <a href={repositoryUrl} target="_blank" rel="noopener noreferrer">
                Source
                <ExternalLink className="size-3.5" />
              </a>
            </Button>
          )}
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={installed || installDisabled}
            onClick={() => onInstall(plugin.name)}
          >
            {installing && <Loader2 className="size-4 animate-spin" />}
            {actionLabel}
          </Button>
        </div>
      </div>
    </article>
  )
}

function CatalogSkeleton() {
  return (
    <div className="grid gap-3 lg:grid-cols-2" aria-hidden="true">
      {[0, 1].map((item) => (
        <div key={item} className="space-y-4 rounded-lg border p-4">
          <div className="flex items-center gap-3">
            <Skeleton className="size-9" />
            <div className="flex-1 space-y-2">
              <Skeleton className="h-4 w-36" />
              <Skeleton className="h-3 w-24" />
            </div>
          </div>
          <Skeleton className="h-4 w-full" />
          <Skeleton className="h-4 w-3/4" />
        </div>
      ))}
    </div>
  )
}

function RunningPluginsSkeleton() {
  return (
    <div className="space-y-2" aria-hidden="true">
      {[0, 1].map((item) => (
        <div
          key={item}
          className="flex items-center gap-3 rounded-lg border p-4"
        >
          <Skeleton className="size-8" />
          <div>
            <Skeleton className="h-4 w-40" />
            <Skeleton className="mt-2 h-3 w-64 max-w-full" />
          </div>
        </div>
      ))}
    </div>
  )
}
