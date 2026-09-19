// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReloadFailureResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
} from '@/components/ui/dropdown-menu'
import { Button } from '@/components/ui/button'
import { PageHeader, Status } from '@temps-sdk/ds'
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@/components/ui/tabs'
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
  Plus,
  Loader2,
  Puzzle,
  RefreshCw,
  Shield,
  MoreHorizontal,
  ArrowUpRight,
  GitPullRequest,
  Trash2,
  Tag,
} from 'lucide-react'
import { createElement, useEffect, useState } from 'react'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import { Link, useNavigate } from 'react-router'
import { toast } from 'sonner'
import { RepositoryCatalog } from '@/components/plugins/RepositoryCatalog'
import { RepositoryUpdate } from '@/components/plugins/RepositoryUpdate'
import { PluginNavigationHint } from '@/components/plugins/PluginNavigationHint'
import { PluginPermissionsDialog } from '@/components/plugins/PluginPermissionsDialog'

export function PluginsPage() {
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const { user } = useAuth()
  const canManagePlugins = canManageExternalPlugins(user?.role)
  const { data: plugins = [], isLoading: pluginsLoading } = usePlugins()
  const reloadPlugins = useReloadPlugins()
  const uninstallPlugin = useUninstallPlugin()
  const [uninstallName, setUninstallName] = useState<string | null>(null)
  const [updateName, setUpdateName] = useState<string | null>(null)
  const [updatePending, setUpdatePending] = useState(false)
  const [permissionsName, setPermissionsName] = useState<string | null>(null)
  const [reloadFailures, setReloadFailures] = useState<ReloadFailureResponse[]>(
    []
  )
  const [tab, setTab] = useState('browse')
  const managementPending = reloadPlugins.isPending || uninstallPlugin.isPending
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
    <div className="w-full min-w-0 space-y-6">
      <Dialog
        open={updateName !== null}
        onOpenChange={(open) => {
          if (!open && !updatePending) setUpdateName(null)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <GitPullRequest className="size-5" /> Update plugin
            </DialogTitle>
            <DialogDescription>
              Choose the revision to build for {updateName}.
            </DialogDescription>
          </DialogHeader>
          <RepositoryUpdate
            name={updateName ?? ''}
            disabled={managementPending}
            onSensitiveError={handleSensitiveActionError}
            onPendingChange={setUpdatePending}
          />
        </DialogContent>
      </Dialog>
      <PluginPermissionsDialog
        name={permissionsName}
        open={canManagePlugins && permissionsName !== null}
        onOpenChange={(open) => {
          if (!open) setPermissionsName(null)
        }}
        onSensitiveError={handleSensitiveActionError}
      />
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
      <PageHeader
        title="Plugins"
        description="Extend Temps with tools for your deployments."
        actions={
          canManagePlugins && (
            <Button
              onClick={() => {
                navigate('/settings/plugins/install')
              }}
              disabled={managementPending}
            >
              <Plus className="mr-2 size-4" /> Install from GitHub
            </Button>
          )
        }
      />
      <Tabs value={tab} onValueChange={setTab} className="space-y-6">
        <TabsList aria-label="Plugin views">
          <TabsTrigger value="browse">Browse</TabsTrigger>
          <TabsTrigger value="running">
            Running{' '}
            <span className="ml-2 text-xs tabular-nums">
              {pluginsLoading ? '…' : plugins.length}
            </span>
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
            canInstall={canManagePlugins}
            disabled={managementPending}
            installedNames={plugins.map((plugin) => plugin.name)}
            onSelect={(plugin) => {
              navigate(
                `/settings/plugins/install?plugin=${encodeURIComponent(plugin.name)}&commit=${plugin.commit}`
              )
            }}
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
                  {plugins.length} verified{' '}
                  {plugins.length === 1 ? 'plugin' : 'plugins'} running
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

            {pluginsLoading ? (
              <RunningPluginsSkeleton />
            ) : plugins.length === 0 ? (
              <div className="rounded-lg border bg-card px-4 py-8 text-center">
                <Puzzle className="mx-auto size-5 text-muted-foreground" />
                <p className="mt-3 font-medium">
                  No verified plugins are running.
                </p>
                <p className="mt-1 text-sm text-muted-foreground">
                  {canManagePlugins
                    ? 'Choose a plugin from the catalog to get started.'
                    : 'Ask a system administrator to install one.'}
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
                          <Status tone="ok" label="Running" />
                        </div>
                        {plugin.description && (
                          <p className="mt-1 text-sm text-muted-foreground">
                            {plugin.description}
                          </p>
                        )}
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
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => setUpdateName(plugin.name)}
                          disabled={managementPending}
                        >
                          <GitPullRequest className="mr-2 size-4" /> Update
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
                              <Trash2 className="mr-2 size-4" /> Uninstall
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
