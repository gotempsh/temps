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
import { EmptyState } from '@/components/ui/empty-state'
import { useMutation, useQuery } from '@tanstack/react-query'
import { BellRing, Pencil, Plus, Trash2 } from 'lucide-react'
import { useMemo } from 'react'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import {
  configuredSlackChannel,
  severityRangeLabel,
} from './notificationRouteUtils'

export function NotificationRoutesManagement() {
  const navigate = useNavigate()
  const {
    data: routePage,
    isLoading: routesLoading,
    refetch: refetchRoutes,
  } = useQuery({
    ...listNotificationRoutesOptions({ query: { page_size: 100 } }),
  })
  const { data: providers, isLoading: providersLoading } = useQuery({
    ...listNotificationProvidersOptions({ query: { page_size: 100 } }),
  })

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

  const routes = routePage?.items || []
  const isLoading = routesLoading || providersLoading
  const startCreate = () => navigate('/settings/notifications/routes/new')
  const startEdit = (routeId: number) =>
    navigate(`/settings/notifications/routes/${routeId}`)

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">
            Notification Routes
          </h2>
          <p className="text-muted-foreground">
            Route severity ranges to provider destinations. Slack channels use
            one webhook provider per channel.
          </p>
        </div>
        {providers && providers.length > 0 && routes.length > 0 && (
          <Button onClick={startCreate}>
            <Plus className="mr-2 h-4 w-4" />
            Add Route
          </Button>
        )}
      </div>

      {!isLoading && providers?.length === 0 ? (
        <EmptyState
          icon={BellRing}
          title="Add a destination first"
          description="Routes send alerts to providers. Add an email, Slack, webhook, or Cloudflare destination before creating a route."
          action={
            <Button onClick={() => navigate('/settings/notifications/new')}>
              Add Provider
            </Button>
          }
        />
      ) : !isLoading && routes.length === 0 ? (
        <EmptyState
          icon={BellRing}
          title="No notification routes configured"
          description="Providers do not receive alert notifications until an enabled route assigns them."
          action={<Button onClick={startCreate}>Create Route</Button>}
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
                    {!route.enabled && (
                      <Badge variant="outline">Disabled</Badge>
                    )}
                  </div>
                </div>
                <div className="flex gap-1">
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
                      deleteMutation.mutate({ path: { id: route.id } })
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
    </div>
  )
}
