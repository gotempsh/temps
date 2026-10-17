// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import {
  deleteNotificationRouteMutation,
  listNotificationProvidersOptions,
  listNotificationRoutesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { NotificationProviderResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { AdminOnlyNote } from '@/components/ui/admin-only-note'
import { EmptyState } from '@/components/ui/empty-state'
import { ReadFailure } from '@/components/ui/read-failure'
import { Skeleton } from '@/components/ui/skeleton'
import { useCurrentUserRole } from '@/hooks/useCurrentUserRole'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { isForbiddenRead } from '@/lib/read-failure'
import { useMutation, useQuery } from '@tanstack/react-query'
import { BellRing, Pencil, Plus, Trash2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useNavigate } from 'react-router'
import { withReturnTo } from '@/lib/safe-return-to'
import { toast } from 'sonner'
import {
  configuredSlackChannel,
  severityRangeLabel,
} from './notificationRouteUtils'
import { RouteTestButton } from './RouteTestButton'

interface NotificationRoutesManagementProps {
  /** Forwarded to route and provider setup so they return to the task. */
  returnTo?: string | null
}

export function NotificationRoutesManagement({
  returnTo = null,
}: NotificationRoutesManagementProps = {}) {
  const navigate = useNavigate()
  const {
    data: routePage,
    isLoading: routesLoading,
    error: routesError,
    isFetching: routesFetching,
    refetch: refetchRoutes,
  } = useQuery({
    ...listNotificationRoutesOptions({ query: { page_size: 100 } }),
  })
  const {
    data: providers,
    isLoading: providersLoading,
    error: providersError,
    isFetching: providersFetching,
    refetch: refetchProviders,
  } = useQuery({
    ...listNotificationProvidersOptions({ query: { page_size: 100 } }),
  })
  // Creating a route or provider is offered to administrators only; for
  // anyone else it would just lead to a refusal.
  const canManage = useIsInstanceAdmin()
  const currentRole = useCurrentUserRole()

  const providerById = useMemo(
    () =>
      new Map(
        (providers || []).map((provider: NotificationProviderResponse) => [
          provider.id,
          provider,
        ])
      ),
    [providers]
  )

  const deleteMutation = useMutation({
    ...deleteNotificationRouteMutation(),
    meta: { errorTitle: 'Failed to delete notification route' },
    onSuccess: () => {
      toast.success('Notification route deleted')
      refetchRoutes()
    },
  })

  const [routeToDelete, setRouteToDelete] = useState<{
    id: number
    name: string
  } | null>(null)
  const handleConfirmDelete = () => {
    if (!routeToDelete) return
    deleteMutation.mutate({ path: { id: routeToDelete.id } })
    setRouteToDelete(null)
  }

  const routes = routePage?.items || []
  const isLoading = routesLoading || providersLoading
  // A failed or refused read is never an empty list.
  const readError =
    (routesError && routes.length === 0 ? routesError : null) ??
    (providersError && !providers?.length ? providersError : null)
  const retryReads = () => {
    if (routesError) void refetchRoutes()
    if (providersError) void refetchProviders()
  }
  const startCreate = () =>
    navigate(withReturnTo('/settings/notifications/routes/new', returnTo))
  const startEdit = (routeId: number) =>
    navigate(
      withReturnTo(`/settings/notifications/routes/${routeId}`, returnTo)
    )

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h2 className="text-base font-semibold">Routes</h2>
          <p className="text-sm text-muted-foreground">
            Routes apply to all projects: every alert whose severity is in a
            route&apos;s range goes to that route&apos;s providers.
          </p>
        </div>
        {canManage &&
          providers &&
          providers.length > 0 &&
          routes.length > 0 && (
            <Button onClick={startCreate}>
              <Plus className="mr-2 h-4 w-4" />
              Add Route
            </Button>
          )}
      </div>

      {!canManage && (
        <AdminOnlyNote action="add or change routes" role={currentRole} />
      )}

      {readError ? (
        <ReadFailure
          resource="Notification routes"
          error={readError}
          onRetry={isForbiddenRead(readError) ? undefined : retryReads}
          retrying={routesFetching || providersFetching}
        />
      ) : isLoading ? (
        <div
          role="status"
          aria-label="Loading notification routes"
          className="space-y-3 rounded-lg border p-4"
        >
          {Array.from({ length: 3 }, (_, index) => (
            <Skeleton key={index} className="h-12 w-full" />
          ))}
        </div>
      ) : providers?.length === 0 ? (
        <EmptyState
          size="compact"
          icon={BellRing}
          title="Add a destination first"
          description="Routes send alerts to providers. Add an email, Slack, webhook, or Cloudflare destination before creating a route."
          action={
            canManage ? (
              <Button
                onClick={() =>
                  navigate(
                    withReturnTo('/settings/notifications/new', returnTo)
                  )
                }
              >
                Add Provider
              </Button>
            ) : undefined
          }
        />
      ) : routes.length === 0 ? (
        <EmptyState
          size="compact"
          icon={BellRing}
          title="No notification routes configured"
          description="Providers do not receive alert notifications until an enabled route assigns them."
          action={
            canManage ? (
              <Button onClick={startCreate}>Create Route</Button>
            ) : undefined
          }
        />
      ) : (
        <div className="grid gap-4 md:grid-cols-2">
          {routes.map((route) => (
            <Card key={route.id}>
              <CardHeader className="flex flex-row items-start justify-between gap-4 space-y-0">
                <div className="space-y-2">
                  <CardTitle className="text-base">{route.name}</CardTitle>
                  <div className="flex flex-wrap gap-2">
                    <Badge variant="secondary">
                      {severityRangeLabel(
                        route.min_severity,
                        route.max_severity
                      )}
                    </Badge>
                    <Badge variant="outline">All projects</Badge>
                    {!route.enabled && (
                      <Badge variant="outline">Disabled</Badge>
                    )}
                  </div>
                </div>
                <div className="flex gap-1">
                  <RouteTestButton
                    routeId={route.id}
                    routeName={route.name}
                    iconOnly
                  />
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={`Edit ${route.name}`}
                    onClick={() => startEdit(route.id)}
                  >
                    <Pencil className="h-4 w-4" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={`Delete ${route.name}`}
                    disabled={deleteMutation.isPending}
                    onClick={() =>
                      setRouteToDelete({ id: route.id, name: route.name })
                    }
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
              </CardHeader>
              <CardContent className="space-y-2">
                {route.provider_ids.map((providerId) => {
                  const provider = providerById.get(providerId)
                  const channel = provider
                    ? configuredSlackChannel(provider)
                    : undefined
                  return (
                    <div
                      key={providerId}
                      className="flex items-center justify-between gap-3 text-sm"
                    >
                      <span className="text-muted-foreground">
                        {provider?.name || `Provider ${providerId}`}
                      </span>
                      {channel && (
                        <Badge variant="outline" className="font-mono">
                          {channel}
                        </Badge>
                      )}
                    </div>
                  )
                })}
              </CardContent>
            </Card>
          ))}
        </div>
      )}
      <ConfirmDeleteDialog
        open={routeToDelete !== null}
        onOpenChange={(open) => {
          if (!open) setRouteToDelete(null)
        }}
        resourceType="notification route"
        resourceName={routeToDelete?.name ?? ''}
        consequences="Alerts matching it will no longer be delivered through it."
        onConfirm={handleConfirmDelete}
      />
    </div>
  )
}
