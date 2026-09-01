// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { ConnectionResponse, ProviderResponse } from '@/api/client/types.gen'
import {
  deleteConnectionMutation,
  runConnectionHealthCheckMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { UpdateTokenDialog } from '@/components/git/UpdateTokenDialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { TimeAgo } from '@/components/utils/TimeAgo'
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  Circle,
  Clock,
  EllipsisVertical,
  HeartPulse,
  Key,
  RefreshCw,
  Trash2,
  Users,
  XCircle,
} from 'lucide-react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'

type Variant = 'single-line' | 'two-line' | 'avatar'

interface ConnectionsCompactListProps {
  connections: ConnectionResponse[]
  provider?: ProviderResponse
  onSyncRepository: (connectionId: number) => void
  isSyncing: boolean
  onConnectionDeleted?: () => void
  variant: Variant
}

export function ConnectionsCompactList({
  connections,
  provider,
  onSyncRepository,
  isSyncing,
  onConnectionDeleted,
  variant,
}: ConnectionsCompactListProps) {
  const queryClient = useQueryClient()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const [updateTokenDialog, setUpdateTokenDialog] = useState<{
    open: boolean
    connectionId: number
    connectionName: string
  }>({ open: false, connectionId: 0, connectionName: '' })
  const [deleteDialog, setDeleteDialog] = useState<{
    open: boolean
    connectionId: number
    connectionName: string
  }>({ open: false, connectionId: 0, connectionName: '' })

  const deleteConnectionMut = useMutation({
    ...deleteConnectionMutation(),
    onSuccess: () => {
      toast.success('Connection deleted successfully')
      queryClient.invalidateQueries({ queryKey: ['listConnections'] })
      setDeleteDialog({ open: false, connectionId: 0, connectionName: '' })
      onConnectionDeleted?.()
    },
    // The server refuses when projects still deploy from this connection and
    // says which ones in the Problem Details `detail`. Swallowing that left the
    // user with a bare "Failed to delete connection" and nothing to act on.
    onError: (error: any, variables) => {
      if (
        handleSensitiveActionError(error, () =>
          deleteConnectionMut.mutate(variables)
        )
      ) {
        setDeleteDialog({ open: false, connectionId: 0, connectionName: '' })
        return
      }
      toast.error('Failed to delete connection', {
        description: error?.detail || error?.title || error?.message,
      })
    },
  })

  const [healthCheckInFlight, setHealthCheckInFlight] = useState<number | null>(
    null
  )
  const healthCheckMut = useMutation({
    ...runConnectionHealthCheckMutation(),
    onMutate: ({ path }) => setHealthCheckInFlight(path.connection_id),
    onSuccess: (data) => {
      if (data.health_status === 'healthy') {
        toast.success(`Connection "${data.account_name}" is healthy`)
      } else if (data.health_status === 'unhealthy') {
        toast.error(
          `Connection "${data.account_name}" is unhealthy`,
          { description: data.health_message ?? undefined }
        )
      } else {
        toast.message(`Health status: ${data.health_status}`)
      }
      queryClient.invalidateQueries({ queryKey: ['listConnections'] })
      onConnectionDeleted?.()
    },
    onError: (error) => {
      toast.error(error?.message ?? 'Health check failed')
    },
    onSettled: () => setHealthCheckInFlight(null),
  })

  const isPATProvider =
    provider &&
    ((provider.provider_type === 'github' &&
      (provider.auth_method === 'pat' ||
        provider.auth_method === 'github_pat')) ||
      (provider.provider_type === 'gitlab' &&
        (provider.auth_method === 'pat' ||
          provider.auth_method === 'gitlab_pat')))

  const ActionsMenu = ({ c }: { c: ConnectionResponse }) => (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" className="h-7 w-7">
          <EllipsisVertical className="h-3.5 w-3.5" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem
          onSelect={(e) => {
            e.preventDefault()
            healthCheckMut.mutate({ path: { connection_id: c.id } })
          }}
          disabled={healthCheckInFlight === c.id}
        >
          <HeartPulse
            className={`mr-2 h-4 w-4 ${
              healthCheckInFlight === c.id ? 'animate-pulse' : ''
            }`}
          />
          Check now
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem
          onSelect={(e) => {
            e.preventDefault()
            onSyncRepository(c.id)
          }}
          disabled={isSyncing}
        >
          <RefreshCw
            className={`mr-2 h-4 w-4 ${isSyncing ? 'animate-spin' : ''}`}
          />
          Sync repositories
        </DropdownMenuItem>
        {isPATProvider && (
          <DropdownMenuItem
            onSelect={(e) => {
              e.preventDefault()
              setUpdateTokenDialog({
                open: true,
                connectionId: c.id,
                connectionName: c.account_name,
              })
            }}
          >
            <Key className="mr-2 h-4 w-4" />
            Update token
          </DropdownMenuItem>
        )}
        {/* Every provider type, not just GitHub Apps: DELETE /git-connections/
            {id} is generic, and hiding it left PAT/OAuth connections with no
            way to be removed — including the ones that block deleting their
            provider. The server still refuses when a project depends on the
            connection, and now says which. */}
        <DropdownMenuSeparator />
        <DropdownMenuItem
          className="text-destructive"
          onSelect={(e) => {
            e.preventDefault()
            setDeleteDialog({
              open: true,
              connectionId: c.id,
              connectionName: c.account_name,
            })
          }}
        >
          <Trash2 className="mr-2 h-4 w-4" />
          Delete connection
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )

  const StatusBadge = ({ c }: { c: ConnectionResponse }) =>
    c.is_active ? (
      <Badge variant="secondary" className="h-5 gap-0.5 px-1.5 text-[10px]">
        <CheckCircle2 className="h-2.5 w-2.5" />
        Active
      </Badge>
    ) : (
      <Badge variant="destructive" className="h-5 gap-0.5 px-1.5 text-[10px]">
        <XCircle className="h-2.5 w-2.5" />
        Inactive
      </Badge>
    )

  const SyncBadge = ({ c }: { c: ConnectionResponse }) => {
    // `synced_repository_count` is served by the backend but hasn't been
    // baked into the generated OpenAPI client types yet — runs of
    // `bun run openapi-ts` after the new server picks it up will expose it
    // typed. Until then, read it off at runtime.
    const count = (c as unknown as { synced_repository_count?: number })
      .synced_repository_count
    return c.syncing ? (
      <Badge variant="outline" className="h-5 gap-0.5 px-1.5 text-[10px]">
        <RefreshCw className="h-2.5 w-2.5 animate-spin" />
        {count && count > 0 ? `Syncing · ${count.toLocaleString()}` : 'Syncing'}
      </Badge>
    ) : null
  }

  const HealthBadge = ({ c }: { c: ConnectionResponse }) => {
    const checkedAt = c.last_health_check_at
    const label = (() => {
      switch (c.health_status) {
        case 'healthy':
          return {
            variant: 'secondary' as const,
            icon: <Activity className="h-2.5 w-2.5 text-emerald-500" />,
            text: 'Healthy',
          }
        case 'unhealthy':
          return {
            variant: 'destructive' as const,
            icon: <AlertTriangle className="h-2.5 w-2.5" />,
            text: 'Unhealthy',
          }
        default:
          return {
            variant: 'outline' as const,
            icon: <Circle className="h-2.5 w-2.5 text-muted-foreground" />,
            text: 'Not checked',
          }
      }
    })()

    const tooltip = (() => {
      if (c.health_status === 'unhealthy' && c.health_message) {
        return c.health_message
      }
      if (!checkedAt) return 'No health check has run yet'
      return `Last checked: ${new Date(checkedAt).toLocaleString()}`
    })()

    return (
      <TooltipProvider delayDuration={150}>
        <Tooltip>
          <TooltipTrigger asChild>
            <Badge
              variant={label.variant}
              className="h-5 cursor-help gap-0.5 px-1.5 text-[10px]"
            >
              {label.icon}
              {label.text}
            </Badge>
          </TooltipTrigger>
          <TooltipContent className="max-w-xs text-xs">
            {tooltip}
          </TooltipContent>
        </Tooltip>
      </TooltipProvider>
    )
  }

  const renderRow = (c: ConnectionResponse) => {
    if (variant === 'single-line') {
      const unhealthy = c.health_status === 'unhealthy'
      const healthy = c.health_status === 'healthy'
      const railColor = unhealthy
        ? 'bg-destructive'
        : healthy
        ? 'bg-emerald-500'
        : 'bg-muted-foreground/30'

      return (
        <li
          key={c.id}
          className="relative flex flex-col gap-1 px-4 py-3 pl-5"
        >
          <span
            aria-hidden
            className={`absolute inset-y-2 left-1.5 w-[3px] rounded-full ${railColor}`}
          />
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <Users className="h-4 w-4 text-muted-foreground shrink-0" />
            <span className="truncate text-sm font-medium">
              {c.account_name}
            </span>
            <Badge variant="outline" className="h-5 px-1.5 text-[10px]">
              {c.account_type || 'unknown'}
            </Badge>
            <StatusBadge c={c} />
            <SyncBadge c={c} />
            <div className="ml-auto flex items-center gap-2">
              <ActionsMenu c={c} />
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-0.5 pl-6 text-xs text-muted-foreground">
            <span
              className={
                unhealthy
                  ? 'font-medium text-destructive'
                  : healthy
                  ? 'text-emerald-600 dark:text-emerald-400'
                  : ''
              }
            >
              {unhealthy && c.health_message
                ? c.health_message
                : healthy
                ? 'Healthy'
                : 'Not checked yet'}
            </span>
            <span aria-hidden>·</span>
            <span className="flex items-center gap-1">
              <Clock className="h-3 w-3" />
              {c.last_health_check_at ? (
                <>
                  checked <TimeAgo date={c.last_health_check_at} />
                </>
              ) : (
                'never checked'
              )}
            </span>
            {c.installation_id && (
              <>
                <span aria-hidden>·</span>
                <span className="font-mono">id:{c.installation_id}</span>
              </>
            )}
          </div>
        </li>
      )
    }

    if (variant === 'two-line') {
      return (
        <li
          key={c.id}
          className="flex items-start gap-3 px-3 py-3 sm:px-4"
        >
          <div className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-md bg-muted">
            <Users className="h-4 w-4 text-muted-foreground" />
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2">
              <span className="truncate text-sm font-medium">
                {c.account_name}
              </span>
              <Badge variant="outline" className="h-5 px-1.5 text-[10px]">
                {c.account_type || 'unknown'}
              </Badge>
              <StatusBadge c={c} />
              <HealthBadge c={c} />
              <SyncBadge c={c} />
            </div>
            <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-muted-foreground">
              {c.installation_id && (
                <span className="font-mono">id:{c.installation_id}</span>
              )}
              <span className="flex items-center gap-1">
                <Clock className="h-3 w-3" />
                {c.last_synced_at ? (
                  <>
                    synced <TimeAgo date={c.last_synced_at} />
                  </>
                ) : (
                  'never synced'
                )}
              </span>
              <span>
                added <TimeAgo date={c.created_at} />
              </span>
            </div>
          </div>
          <ActionsMenu c={c} />
        </li>
      )
    }

    // variant === 'avatar'
    const initials = c.account_name
      .split(/[\s-]+/)
      .map((s) => s[0])
      .slice(0, 2)
      .join('')
      .toUpperCase()

    return (
      <li
        key={c.id}
        className="flex items-center gap-3 px-3 py-2.5 sm:px-4"
      >
        <div className="flex size-9 shrink-0 items-center justify-center rounded-full border bg-muted text-xs font-semibold text-muted-foreground">
          {initials || '?'}
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate text-sm font-medium">
              {c.account_name}
            </span>
            <Badge variant="outline" className="h-5 px-1.5 text-[10px]">
              {c.account_type || 'unknown'}
            </Badge>
          </div>
          <div className="mt-0.5 flex items-center gap-3 text-xs text-muted-foreground">
            <span className="flex items-center gap-1">
              <Clock className="h-3 w-3" />
              {c.last_synced_at ? (
                <>
                  synced <TimeAgo date={c.last_synced_at} />
                </>
              ) : (
                'never synced'
              )}
            </span>
            {c.installation_id && (
              <span className="font-mono truncate">
                id:{c.installation_id}
              </span>
            )}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <StatusBadge c={c} />
          <HealthBadge c={c} />
          <SyncBadge c={c} />
          <ActionsMenu c={c} />
        </div>
      </li>
    )
  }

  return (
    <>
      <ul role="list" className="divide-y rounded-md border">
        {connections.map(renderRow)}
      </ul>

      <UpdateTokenDialog
        connectionId={updateTokenDialog.connectionId}
        connectionName={updateTokenDialog.connectionName}
        open={updateTokenDialog.open}
        onOpenChange={(open) =>
          setUpdateTokenDialog({ ...updateTokenDialog, open })
        }
      />

      {verificationDialog}

      <AlertDialog
        open={deleteDialog.open}
        onOpenChange={(open) => setDeleteDialog({ ...deleteDialog, open })}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete Connection</AlertDialogTitle>
            <AlertDialogDescription>
              Are you sure you want to delete the connection for{' '}
              <strong>{deleteDialog.connectionName}</strong>? This action
              cannot be undone and will remove all associated repositories.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleteConnectionMut.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={() =>
                deleteConnectionMut.mutate({
                  path: { connection_id: deleteDialog.connectionId },
                })
              }
              disabled={deleteConnectionMut.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {deleteConnectionMut.isPending ? (
                <>
                  <RefreshCw className="mr-2 h-4 w-4 animate-spin" />
                  Deleting...
                </>
              ) : (
                <>
                  <Trash2 className="mr-2 h-4 w-4" />
                  Delete
                </>
              )}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  )
}
