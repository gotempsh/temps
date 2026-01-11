'use client'

import {
  deleteRoute,
  ListRoutesResponse,
  RouteResponse,
  updateRoute,
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
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { Globe, Lock, MoreHorizontal, Pencil, Plus, Router, Trash2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'

interface RoutesManagementProps {
  routes?: ListRoutesResponse
  isLoading: boolean
  reloadRoutes: () => void
}

const editRouteSchema = z.object({
  domain: z.string().min(1, 'Domain is required'),
  host: z.string().min(1, 'Host is required'),
  port: z.number().min(1, 'Port is required').max(65535, 'Port must be at most 65535'),
})

type EditRouteFormData = z.infer<typeof editRouteSchema>

export function RoutesManagement({
  routes,
  isLoading,
  reloadRoutes,
}: RoutesManagementProps) {
  const [routeToDelete, setRouteToDelete] = useState<string | null>(null)
  const [editRoute, setEditRoute] = useState<RouteResponse | null>(null)

  const updateRouteMutation = useMutation({
    mutationFn: (data: { domain: string; host: string; port: number }) =>
      updateRoute({
        path: { domain: data.domain },
        body: {
          enabled: true,
          host: data.host,
          port: data.port,
        },
      }),
    meta: {
      errorTitle: 'Failed to update route',
    },
    onSuccess: ({ response }: any) => {
      if (response.status >= 300) {
        toast.error('Failed to update route')
      } else {
        setEditRoute(null)
        toast.success('Route updated successfully')
        reloadRoutes()
      }
    },
  })

  const deleteRouteMutation = useMutation({
    mutationFn: (domain: string) => deleteRoute({ path: { domain } }),
    meta: {
      errorTitle: 'Failed to delete route',
    },
    onSuccess: () => {
      toast.success('Route deleted successfully')
      reloadRoutes()
      setRouteToDelete(null)
    },
  })

  const editForm = useForm<EditRouteFormData>({
    resolver: zodResolver(editRouteSchema),
    defaultValues: {
      domain: '',
      host: '',
      port: 80,
    },
  })

  useEffect(() => {
    if (editRoute) {
      editForm.reset({
        domain: editRoute.domain,
        host: editRoute.host,
        port: editRoute.port,
      })
    }
  }, [editRoute, editForm])

  const onEditSubmit = async (data: EditRouteFormData) => {
    await updateRouteMutation.mutateAsync({
      domain: data.domain,
      host: data.host,
      port: data.port,
    })
    editForm.reset()
  }

  const formatRouteType = (routeType: string) => {
    if (routeType === 'tls') {
      return (
        <Badge variant="outline" className="gap-1">
          <Lock className="h-3 w-3" />
          TLS/SNI
        </Badge>
      )
    }
    return (
      <Badge variant="secondary" className="gap-1">
        <Globe className="h-3 w-3" />
        HTTP
      </Badge>
    )
  }
  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">Routes Management</h2>
          <p className="text-sm text-muted-foreground">
            Configure custom domain routing and load balancing
          </p>
        </div>
        <Button asChild>
          <Link to="/load-balancer/add">
            <Plus className="mr-2 h-4 w-4" />
            Add Route
          </Link>
        </Button>
      </div>

      {isLoading ? (
        <Card>
          <div className="p-4 space-y-4">
            {[...Array(3)].map((_, i) => (
              <div
                key={i}
                className="flex items-center justify-between py-4 animate-pulse"
              >
                <div className="space-y-2">
                  <div className="h-4 w-48 bg-muted rounded" />
                  <div className="h-4 w-32 bg-muted rounded" />
                </div>
                <div className="h-8 w-8 bg-muted rounded" />
              </div>
            ))}
          </div>
        </Card>
      ) : !routes?.length ? (
        <EmptyState
          icon={Router}
          title="No routes configured"
          description="Get started by adding a new route to direct traffic to your backend services"
          action={
            <Button asChild>
              <Link to="/load-balancer/add">
                <Plus className="mr-2 h-4 w-4" />
                Add Route
              </Link>
            </Button>
          }
        />
      ) : (
        <Card>
          <div className="divide-y">
            {routes.map((route) => (
              <div
                key={route.id}
                className="flex items-center justify-between p-4"
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-3">
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <h3 className="truncate font-medium">{route.domain}</h3>
                        {formatRouteType(route.route_type)}
                      </div>
                      <p className="text-sm text-muted-foreground mt-0.5">
                        <span className="font-mono">
                          {route.host}:{route.port}
                        </span>
                      </p>
                    </div>
                  </div>
                </div>
                <div className="ml-4">
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button variant="ghost" size="icon">
                        <MoreHorizontal className="h-4 w-4" />
                        <span className="sr-only">Open menu</span>
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem onClick={() => setEditRoute(route)}>
                        <Pencil className="mr-2 h-4 w-4" />
                        Edit
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        onClick={() => setRouteToDelete(route.domain)}
                        className="text-destructive"
                      >
                        <Trash2 className="mr-2 h-4 w-4" />
                        Delete
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              </div>
            ))}
          </div>
        </Card>
      )}

      {/* Edit Dialog */}
      <Dialog
        open={!!editRoute}
        onOpenChange={(open) => !open && setEditRoute(null)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Edit Route</DialogTitle>
          </DialogHeader>
          <Form {...editForm}>
            <form
              onSubmit={editForm.handleSubmit(onEditSubmit)}
              className="space-y-6"
            >
              <FormField
                control={editForm.control}
                name="domain"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Domain</FormLabel>
                    <FormControl>
                      <Input disabled {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={editForm.control}
                name="host"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Host</FormLabel>
                    <FormControl>
                      <Input placeholder="localhost or IP address" {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={editForm.control}
                name="port"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Port</FormLabel>
                    <FormControl>
                      <Input
                        type="number"
                        placeholder="8080"
                        min={1}
                        max={65535}
                        value={field.value}
                        onChange={(e) => field.onChange(Number(e.target.value) || 0)}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <div className="flex justify-end">
                <Button type="submit" disabled={updateRouteMutation.isPending}>
                  {updateRouteMutation.isPending ? 'Saving...' : 'Save Changes'}
                </Button>
              </div>
            </form>
          </Form>
        </DialogContent>
      </Dialog>

      {/* Delete Confirmation Dialog */}
      <AlertDialog
        open={routeToDelete !== null}
        onOpenChange={(open) => !open && setRouteToDelete(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Are you sure?</AlertDialogTitle>
            <AlertDialogDescription>
              This action cannot be undone. This will permanently delete the
              route configuration.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() =>
                routeToDelete && deleteRouteMutation.mutate(routeToDelete)
              }
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={deleteRouteMutation.isPending}
            >
              {deleteRouteMutation.isPending ? 'Deleting...' : 'Delete'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
