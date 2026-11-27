// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ReloadFailureResponse,
  ServicePluginResponse,
} from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
} from '@/components/ui/dropdown-menu'
import { Button } from '@/components/ui/button'
import { PageHeader, Status, useUrlState } from '@temps-sdk/ds'
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@/components/ui/tabs'
import { Skeleton } from '@/components/ui/skeleton'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { useAuth } from '@/contexts/AuthContext'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  usePluginServices,
  usePlugins,
  useReloadPlugins,
  useUninstallPlugin,
} from '@/hooks/usePlugins'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import {
  canManageExternalPlugins,
  pluginReloadFailures,
} from '@/lib/plugin-registry'
import {
  Activity,
  AlertCircle,
  ArrowUpRight,
  Compass,
  MoreHorizontal,
  Plus,
  Puzzle,
  RefreshCw,
  Shield,
  Tag,
  Trash2,
} from 'lucide-react'
import { createElement, useEffect, useState } from 'react'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { RepositoryCatalog } from '@/components/plugins/RepositoryCatalog'
import { PluginNavigationHint } from '@/components/plugins/PluginNavigationHint'
import { PluginPermissionsDialog } from '@/components/plugins/PluginPermissionsDialog'
import {
  AddServicePluginDialog,
  RetiredPluginNotice,
  ServicePluginNotice,
  ServicePluginOrigin,
  ServicePluginStatus,
} from '@/components/plugins/ServicePlugins'

export function PluginsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { user } = useAuth()
  const canManagePlugins = canManageExternalPlugins(user?.role)
  const { data: plugins = [], isLoading: pluginsLoading } = usePlugins()
  const reloadPlugins = useReloadPlugins()
  const uninstallPlugin = useUninstallPlugin()
  // Plugins that run as Temps services, and how each is doing.
  const services = usePluginServices(canManagePlugins)
  const serviceOf = (name: string) =>
    services.data?.plugins.find((plugin) => plugin.name === name)
  const [addingService, setAddingService] = useState(false)
  const [uninstallName, setUninstallName] = useState<string | null>(null)
  const [permissionsName, setPermissionsName] = useState<string | null>(null)
  const [reloadFailures, setReloadFailures] = useState<ReloadFailureResponse[]>(
    []
  )
  // The active tab lives in the URL (RULES.md "the URL is the state") so a
  // refresh, a shared link, or Back keeps the same view. Browse is the
  // default and is left out of the URL.
  const { get: getUrlState, patch: patchUrlState } = useUrlState<'tab'>()
  const tab = getUrlState('tab') === 'running' ? 'running' : 'browse'
  const setTab = (value: string) =>
    patchUrlState({ tab: value === 'browse' ? undefined : value })
  const managementPending = reloadPlugins.isPending || uninstallPlugin.isPending
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  // An entry left by a plugin an earlier version installed as an executable.
  const removesRetired =
    uninstallName !== null &&
    (services.data?.retired ?? []).some(
      (plugin) => plugin.name === uninstallName
    )

  useEffect(() => {
    setBreadcrumbs([{ label: 'Plugins' }])
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
      // The toast comes from the app-wide mutation error handler.
      setReloadFailures(pluginReloadFailures(error))
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
    }
  }

  return (
    <div className="w-full min-w-0 space-y-6">
      <PluginPermissionsDialog
        name={permissionsName}
        open={canManagePlugins && permissionsName !== null}
        onOpenChange={(open) => {
          if (!open) setPermissionsName(null)
        }}
        onSensitiveError={handleSensitiveActionError}
      />
      <AddServicePluginDialog
        open={canManagePlugins && addingService}
        onOpenChange={setAddingService}
        services={services.data}
        onSensitiveError={handleSensitiveActionError}
      />
      {canManagePlugins && verificationDialog}
      <ConfirmDeleteDialog
        open={canManagePlugins && uninstallName !== null}
        onOpenChange={(open) => {
          if (!open) setUninstallName(null)
        }}
        verb="Remove"
        resourceType="plugin"
        resourceName={uninstallName ?? ''}
        irreversible={false}
        consequences={
          removesRetired
            ? 'This removes the entry and revokes the permissions the plugin had. The files it stored on this server are kept.'
            : 'This removes the plugin from navigation and revokes its permissions. The service it runs as keeps running; stop or delete it from its own page.'
        }
        isPending={uninstallPlugin.isPending}
        onConfirm={() => {
          if (uninstallName) void handleUninstall(uninstallName)
        }}
      />
      <PageHeader
        title="Plugins"
        description="Extend Temps with tools for your deployments."
        actions={
          canManagePlugins && (
            <Button
              disabled={managementPending}
              onClick={() => setAddingService(true)}
            >
              <Plus className="mr-2 size-4" /> Add from service
            </Button>
          )
        }
      />
      {canManagePlugins && (
        <RetiredPluginNotice
          retired={services.data?.retired}
          disabled={managementPending}
          onRemove={setUninstallName}
        />
      )}
      <Tabs value={tab} onValueChange={setTab} className="space-y-6">
        <TabsList aria-label="Plugin views">
          <TabsTrigger value="browse" icon={Compass}>
            Browse
          </TabsTrigger>
          <TabsTrigger
            value="running"
            icon={Activity}
            count={pluginsLoading ? undefined : plugins.length}
          >
            Running
          </TabsTrigger>
        </TabsList>
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
        <TabsContent value="browse" className="space-y-4">
          <RepositoryCatalog
            addedNames={plugins.map((plugin) => plugin.name)}
          />
        </TabsContent>
        <TabsContent value="running">
          <section
            className="space-y-3"
            aria-labelledby="running-plugins-title"
          >
            <div className="flex items-center justify-between gap-4">
              <div>
                <h2 id="running-plugins-title" className="sr-only">
                  Running
                </h2>
                <p className="text-sm text-muted-foreground">
                  {plugins.length} {plugins.length === 1 ? 'plugin' : 'plugins'}{' '}
                  added
                </p>
              </div>
              {canManagePlugins && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={handleReload}
                  disabled={managementPending}
                >
                  <RefreshCw
                    className={`mr-2 size-4 ${reloadPlugins.isPending ? 'animate-spin' : ''}`}
                  />
                  {reloadPlugins.isPending ? 'Reloading…' : 'Reload plugins'}
                </Button>
              )}
            </div>

            {canManagePlugins && (
              <ServicePluginNotice
                services={services.data}
                unavailable={services.isError}
                disabled={managementPending}
                onRemove={setUninstallName}
              />
            )}

            {pluginsLoading ? (
              <RunningPluginsSkeleton />
            ) : plugins.length === 0 ? (
              <div className="rounded-lg border bg-card px-4 py-8 text-center">
                <Puzzle className="mx-auto size-5 text-muted-foreground" />
                <p className="mt-3 font-medium">No plugins are added.</p>
                <p className="mt-1 text-sm text-muted-foreground">
                  {canManagePlugins
                    ? 'Deploy a plugin as a service, then add that service here.'
                    : 'Ask a system administrator to add one.'}
                </p>
                <Button
                  variant="outline"
                  size="sm"
                  className="mt-4"
                  onClick={() => setTab('browse')}
                >
                  Browse plugins
                </Button>
              </div>
            ) : (
              <div className="divide-y rounded-lg border bg-card">
                {plugins.map((plugin) => (
                  <div
                    key={plugin.name}
                    className="flex flex-col gap-4 p-4 sm:flex-row sm:items-center sm:justify-between"
                  >
                    <div className="flex min-w-0 items-start gap-3">
                      <div className="flex size-10 shrink-0 items-center justify-center rounded-lg border bg-muted/40">
                        {createElement(
                          resolvePluginIcon(plugin.nav[0]?.icon ?? 'puzzle'),
                          { className: 'size-5 text-muted-foreground' }
                        )}
                      </div>
                      <div className="min-w-0">
                        <div className="flex flex-wrap items-center gap-2">
                          <p className="font-medium">
                            {plugin.display_name || plugin.name}
                          </p>
                          <span className="flex items-center gap-1 text-xs text-muted-foreground">
                            <Tag className="size-3" /> v{plugin.version}
                          </span>
                          <PluginRunState service={serviceOf(plugin.name)} />
                        </div>
                        {plugin.description && (
                          <p className="mt-1 text-sm text-muted-foreground">
                            {plugin.description}
                          </p>
                        )}
                        <PluginServiceOrigin service={serviceOf(plugin.name)} />
                        <PluginNavigationHint nav={plugin.nav} />
                      </div>
                    </div>
                    <div className="flex min-w-0 flex-wrap items-center gap-2">
                      {plugin.nav.some(
                        (entry) => entry.section !== 'project'
                      ) && (
                        <Button asChild variant="outline" size="sm">
                          <Link to={`/plugins/${plugin.name}`}>
                            Open <ArrowUpRight className="ml-1 size-4" />
                          </Link>
                        </Button>
                      )}
                      {canManagePlugins && (
                        <DropdownMenu>
                          <DropdownMenuTrigger asChild>
                            <Button
                              variant="ghost"
                              size="icon"
                              aria-label={`Actions for ${plugin.display_name || plugin.name}`}
                              disabled={managementPending}
                            >
                              <MoreHorizontal className="size-4" />
                            </Button>
                          </DropdownMenuTrigger>
                          <DropdownMenuContent align="end">
                            <DropdownMenuItem
                              onSelect={() => setPermissionsName(plugin.name)}
                            >
                              <Shield className="mr-2 size-4" /> Permissions
                            </DropdownMenuItem>
                            <DropdownMenuSeparator />
                            <DropdownMenuItem
                              className="text-destructive focus:text-destructive"
                              onSelect={() => setUninstallName(plugin.name)}
                            >
                              <Trash2 className="mr-2 size-4" /> Remove
                            </DropdownMenuItem>
                          </DropdownMenuContent>
                        </DropdownMenu>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </section>
        </TabsContent>
      </Tabs>
    </div>
  )
}

/** Running, unless the plugin's service is stopped or unreachable. */
function PluginRunState({
  service,
}: {
  service: ServicePluginResponse | undefined
}) {
  if (!service) return <Status tone="ok" label="Running" />
  return <ServicePluginStatus plugin={service} />
}

/** The service a plugin runs as. */
function PluginServiceOrigin({
  service,
}: {
  service: ServicePluginResponse | undefined
}) {
  if (!service) return null
  return <ServicePluginOrigin plugin={service} />
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
