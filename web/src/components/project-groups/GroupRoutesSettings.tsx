// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { AlertTriangle, ArrowRight, Loader2, Trash2 } from 'lucide-react'
import {
  createGroupRouteMutation,
  deleteGroupRouteMutation,
  listGroupRoutesOptions,
  listGroupRoutesQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  EnvironmentResponse,
  GroupRouteResponse,
  ProjectGroupResponse,
} from '@/api/client'
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
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { getErrorMessage } from '@/utils/errorHandling'
import { RouteAddresses } from './GroupRoutesSummary'
import { useServiceEnvironments } from './service-urls'

/**
 * Path routes (ADR 050): a service's domains hand a path such as `/api` to
 * another service of the project, so an app that calls a same-origin API
 * works without CORS or a second domain.
 */
export function GroupRoutesSettings({
  group,
}: {
  group: ProjectGroupResponse
}) {
  const queryClient = useQueryClient()
  const path = { group_id: group.id }
  const routes = useQuery(listGroupRoutesOptions({ path }))
  const create = useMutation(createGroupRouteMutation())
  const remove = useMutation(deleteGroupRouteMutation())

  const services = group.projects
  const environments = useServiceEnvironments(services.map((p) => p.id))
  const [source, setSource] = useState('')
  const [target, setTarget] = useState('')
  const [prefix, setPrefix] = useState('/api')
  const [strip, setStrip] = useState(false)
  const [formError, setFormError] = useState<string | null>(null)
  // Removing a route changes live traffic, so it is confirmed first.
  const [confirming, setConfirming] = useState<GroupRouteResponse | null>(null)

  const refresh = () =>
    queryClient.invalidateQueries({
      queryKey: listGroupRoutesQueryKey({ path }),
    })

  const add = async (e: React.FormEvent) => {
    e.preventDefault()
    setFormError(null)
    try {
      const created = await create.mutateAsync({
        path,
        body: {
          source_project_id: Number(source),
          target_project_id: Number(target),
          path_prefix: prefix,
          strip_prefix: strip,
        },
      })
      await refresh()
      toast.success(
        `${created.source_project_name} now sends ${created.path_prefix} to ${created.target_project_name}`
      )
      setPrefix('/api')
      setStrip(false)
    } catch (err) {
      setFormError(getErrorMessage(err, 'The route could not be added.'))
    }
  }

  const removeRoute = async (route: GroupRouteResponse) => {
    try {
      await remove.mutateAsync({
        path: { group_id: group.id, route_id: route.id },
      })
      await refresh()
      toast.success(
        `${route.source_project_name} serves ${route.path_prefix} itself again`
      )
    } catch (err) {
      toast.error(getErrorMessage(err, 'The route could not be removed.'))
    }
  }

  return (
    <div className="space-y-3">
      <div>
        <p className="text-sm text-muted-foreground">
          Serve a path of one service&apos;s domains from another service, e.g.{' '}
          <code>/api</code> on the frontend&apos;s domain from the API. Each
          environment goes to the other service&apos;s environment of the same
          name (previews: same branch). Services keep their own domains too.
        </p>
        <p className="mt-1 text-sm text-muted-foreground">
          Routed requests get the serving service&apos;s protections (password,
          attack mode, limits), not this service&apos;s: protect the API itself
          if it needs it.
        </p>
      </div>

      {routes.isLoading ? (
        <div
          role="status"
          className="divide-y rounded-lg border"
          aria-busy="true"
          aria-label="Loading routes"
        >
          {[0, 1].map((i) => (
            <div key={i} className="flex items-center gap-2 px-3 py-2.5">
              <Skeleton className="h-4 w-24" />
              <Skeleton className="h-4 w-12" />
              <Skeleton className="h-4 w-24" />
            </div>
          ))}
        </div>
      ) : routes.isError ? (
        <div
          role="alert"
          className="flex items-center gap-2 text-sm text-destructive"
        >
          <AlertTriangle className="size-4 shrink-0" />
          <span className="flex-1">
            {getErrorMessage(routes.error, 'Routes could not be loaded.')}
          </span>
          <Button variant="outline" size="sm" onClick={() => routes.refetch()}>
            Retry
          </Button>
        </div>
      ) : (routes.data ?? []).length > 0 ? (
        <div className="divide-y rounded-lg border">
          {(routes.data ?? []).map((route) => (
            <RouteRow
              key={route.id}
              route={route}
              slugOf={(id) => services.find((p) => p.id === id)?.slug}
              environmentsById={environments.byId}
              removing={
                remove.isPending && remove.variables?.path.route_id === route.id
              }
              onRemove={() => setConfirming(route)}
            />
          ))}
        </div>
      ) : null}

      <AlertDialog
        open={confirming !== null}
        onOpenChange={(open) => !open && setConfirming(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Remove the {confirming?.path_prefix} route?
            </AlertDialogTitle>
            <AlertDialogDescription>
              Requests to {confirming?.path_prefix} on{' '}
              {confirming?.source_project_name}&apos;s domains stop going to{' '}
              {confirming?.target_project_name} right away and are answered by{' '}
              {confirming?.source_project_name} itself. Clients that call that
              path may start failing.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                const route = confirming
                setConfirming(null)
                if (route) void removeRoute(route)
              }}
            >
              Remove route
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {services.length < 2 ? (
        <p className="rounded-lg border border-dashed px-4 py-3 text-sm text-muted-foreground">
          Routes connect two services of this project. Add a second service
          (say, an API next to a frontend) to route a path between them.
        </p>
      ) : (
        <form
          onSubmit={add}
          className="space-y-3 rounded-lg border border-dashed px-4 py-4"
        >
          <div className="grid gap-3 sm:grid-cols-[1fr_8rem_1fr]">
            <div className="space-y-1.5">
              <Label htmlFor="route-source">On the domains of</Label>
              <Select value={source} onValueChange={setSource}>
                <SelectTrigger id="route-source">
                  <SelectValue placeholder="Service" />
                </SelectTrigger>
                <SelectContent>
                  {services.map((p) => (
                    <SelectItem key={p.id} value={String(p.id)}>
                      {p.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="route-prefix">Path</Label>
              <Input
                id="route-prefix"
                className="font-mono"
                value={prefix}
                onChange={(e) => setPrefix(e.target.value)}
                placeholder="/api"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="route-target">Is served by</Label>
              <Select value={target} onValueChange={setTarget}>
                <SelectTrigger id="route-target">
                  <SelectValue placeholder="Service" />
                </SelectTrigger>
                <SelectContent>
                  {services
                    .filter((p) => String(p.id) !== source)
                    .map((p) => (
                      <SelectItem key={p.id} value={String(p.id)}>
                        {p.name}
                      </SelectItem>
                    ))}
                </SelectContent>
              </Select>
            </div>
          </div>
          <div className="flex items-center gap-2">
            <Switch
              id="route-strip"
              checked={strip}
              onCheckedChange={setStrip}
            />
            <Label htmlFor="route-strip" className="font-normal">
              Remove the path before forwarding (
              <code>{prefix || '/api'}/users</code> arrives as{' '}
              <code>/users</code>)
            </Label>
          </div>
          {formError && (
            <p role="alert" className="text-sm font-medium text-destructive">
              {formError}
            </p>
          )}
          <Button
            type="submit"
            size="sm"
            disabled={
              !source || !target || source === target || create.isPending
            }
          >
            {create.isPending ? 'Adding…' : 'Add route'}
          </Button>
        </form>
      )}
    </div>
  )
}

function RouteRow({
  route,
  slugOf,
  environmentsById,
  removing,
  onRemove,
}: {
  route: GroupRouteResponse
  slugOf: (id: number) => string | undefined
  environmentsById: Map<number, EnvironmentResponse>
  removing: boolean
  onRemove: () => void
}) {
  const service = (id: number, name: string) => {
    const slug = slugOf(id)
    return slug ? (
      <Link to={`/projects/${slug}`} className="font-medium hover:underline">
        {name}
      </Link>
    ) : (
      <span className="font-medium">{name}</span>
    )
  }
  return (
    <div className="space-y-1 px-3 py-2">
      <div className="flex items-center gap-2 text-sm">
        {service(route.source_project_id, route.source_project_name)}
        <code className="rounded bg-muted px-1.5 py-0.5 text-xs">
          {route.path_prefix}
        </code>
        <ArrowRight
          className="size-3.5 text-muted-foreground"
          aria-hidden="true"
        />
        <span className="min-w-0 flex-1 truncate">
          {service(route.target_project_id, route.target_project_name)}
        </span>
        {route.strip_prefix && (
          <span className="text-xs text-muted-foreground">prefix removed</span>
        )}
        <Button
          variant="ghost"
          size="sm"
          aria-label={`Remove route ${route.path_prefix}`}
          disabled={removing}
          onClick={onRemove}
        >
          {removing ? (
            <Loader2 className="size-4 animate-spin" />
          ) : (
            <Trash2 className="size-4" />
          )}
        </Button>
      </div>
      <RouteAddresses route={route} environmentsById={environmentsById} />
    </div>
  )
}
