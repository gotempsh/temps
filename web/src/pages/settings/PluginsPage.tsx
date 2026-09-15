// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReloadFailureResponse } from '@/api/client/types.gen'
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
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
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
  usePlugins,
  useReloadPlugins,
  useUninstallPlugin,
} from '@/hooks/usePlugins'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import {
  canManageExternalPlugins,
  pluginReloadFailures,
} from '@/lib/plugin-registry'
import { sensitiveActionErrorMessage } from '@/lib/sensitiveActionProblem'
import {
  AlertCircle,
  ChevronDown,
  Loader2,
  Puzzle,
  RefreshCw,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import {
  RepositoryInstall,
  type RepositorySelection,
} from '@/components/plugins/RepositoryInstall'
import { RepositoryCatalog } from '@/components/plugins/RepositoryCatalog'
import { RepositoryUpdate } from '@/components/plugins/RepositoryUpdate'
import { PluginNavigationHint } from '@/components/plugins/PluginNavigationHint'

export function PluginsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { user } = useAuth()
  const canManagePlugins = canManageExternalPlugins(user?.role)
  const { data: plugins = [], isLoading: pluginsLoading } = usePlugins()
  const reloadPlugins = useReloadPlugins()
  const uninstallPlugin = useUninstallPlugin()
  const [uninstallName, setUninstallName] = useState<string | null>(null)
  const [reloadFailures, setReloadFailures] = useState<ReloadFailureResponse[]>(
    []
  )
  const [catalogSelection, setCatalogSelection] =
    useState<RepositorySelection | null>(null)
  const [repositoryInstalling, setRepositoryInstalling] = useState(false)
  const [advancedOpen, setAdvancedOpen] = useState(false)
  const managementPending =
    reloadPlugins.isPending || uninstallPlugin.isPending || repositoryInstalling
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
              <CardTitle>Plugins</CardTitle>
              <CardDescription>
                Discover plugins for your Temps instance.
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
          <RepositoryCatalog
            canInstall={canManagePlugins}
            disabled={managementPending}
            installedNames={plugins.map((plugin) => plugin.name)}
            onSelect={(plugin) => {
              setAdvancedOpen(false)
              setCatalogSelection(plugin)
            }}
          />
          {canManagePlugins && catalogSelection && (
            <RepositoryInstall
              disabled={managementPending}
              onSensitiveError={handleSensitiveActionError}
              selection={catalogSelection}
              onClearSelection={() => setCatalogSelection(null)}
              onPendingChange={setRepositoryInstalling}
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
                  Plugins currently loaded by Temps.
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
                <p className="mt-3 font-medium">No plugins are running.</p>
                <p className="mt-1 text-sm text-muted-foreground">
                  {canManagePlugins
                    ? 'Choose a plugin from the catalog to get started.'
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
                      <PluginNavigationHint nav={plugin.nav} />
                    </div>
                    <div className="flex items-center gap-2">
                      {canManagePlugins && (
                        <RepositoryUpdate
                          name={plugin.name}
                          disabled={managementPending}
                          onSensitiveError={handleSensitiveActionError}
                        />
                      )}
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
          {canManagePlugins && !catalogSelection && (
            <Collapsible
              open={advancedOpen}
              onOpenChange={(open) => {
                if (!managementPending) setAdvancedOpen(open)
              }}
              className="border-t pt-4"
            >
              <CollapsibleTrigger asChild>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  disabled={managementPending}
                  className="group gap-2"
                >
                  Advanced
                  <ChevronDown
                    aria-hidden="true"
                    className="size-4 shrink-0 transition-transform group-data-[state=open]:rotate-180"
                  />
                </Button>
              </CollapsibleTrigger>
              <CollapsibleContent className="pt-4">
                <RepositoryInstall
                  disabled={managementPending}
                  onSensitiveError={handleSensitiveActionError}
                  onPendingChange={setRepositoryInstalling}
                />
              </CollapsibleContent>
            </Collapsible>
          )}
        </CardContent>
      </Card>
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
