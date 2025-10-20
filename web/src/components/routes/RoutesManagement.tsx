'use client'

import {
  createRoute,
  deleteRoute,
  ListDomainsResponse,
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
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
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
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { usePlatformCapabilities } from '@/hooks/usePlatformCapabilities'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { MoreHorizontal, Pencil, Plus, Router, Trash2 } from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

interface NewRoute {
  domain: string
  host: string
  port: number
  domainInputType: 'select' | 'manual'
  subdomain?: string
}

interface RoutesManagementProps {
  routes?: ListRoutesResponse
  domains?: ListDomainsResponse
  isLoading: boolean
  reloadRoutes: () => void
}

const createRouteSchema = z
  .object({
    domain: z.string().min(1, 'Domain is required'),
    host: z.string().min(1, 'Host is required'),
    port: z.number().min(1, 'Port is required'),
    domainInputType: z.enum(['select', 'manual']),
    subdomain: z.string().optional(),
  })
  .refine(
    (data) => {
      // If domain is a wildcard, subdomain is required
      if (data.domain && data.domain.includes('*.')) {
        return data.subdomain && data.subdomain.trim().length > 0
      }
      return true
    },
    {
      message: 'Subdomain is required for wildcard domains',
      path: ['subdomain'],
    }
  )

type CreateRouteFormData = z.infer<typeof createRouteSchema>

export function RoutesManagement({
  routes,
  domains,
  isLoading,
  reloadRoutes,
}: RoutesManagementProps) {
  const [isCreateDialogOpen, setIsCreateDialogOpen] = useState(false)
  const [routeToDelete, setRouteToDelete] = useState<string | null>(null)

  const [editRoute, setEditRoute] = useState<RouteResponse | null>(null)
  const { accessMode } = usePlatformCapabilities()
  const isLocalMode = useMemo(() => accessMode === 'local', [accessMode])
  // Check if domains are available
  const hasAvailableDomains = domains?.domains && domains.domains.length > 0

  const form = useForm<CreateRouteFormData>({
    resolver: zodResolver(createRouteSchema),
    defaultValues: {
      domain: '',
      host: '',
      port: 80,
      domainInputType:
        isLocalMode || !hasAvailableDomains ? 'manual' : 'select',
      subdomain: '',
    },
  })

  // Mutations setup
  const createRouteMutation = useMutation({
    mutationFn: (route: NewRoute) => createRoute({ body: route }),
    meta: {
      errorTitle: 'Failed to create route',
    },
    onSuccess: ({ response }: any) => {
      if (response.status >= 300) {
        toast.error('Failed to create route')
      } else {
        form.reset({
          domain: '',
          host: '',
          port: 80,
          domainInputType:
            isLocalMode || !hasAvailableDomains ? 'manual' : 'select',
          subdomain: '',
        })
        setIsCreateDialogOpen(false)
        toast.success('Route created successfully')
        reloadRoutes()
      }
    },
  })

  const updateRouteMutation = useMutation({
    mutationFn: (route: NewRoute) =>
      updateRoute({
        path: { domain: route.domain },
        body: {
          enabled: true,
          host: route.host,
          port: route.port,
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
  const editForm = useForm<CreateRouteFormData>({
    resolver: zodResolver(createRouteSchema),
    defaultValues: {
      domain: '',
      host: '',
      port: 80,
      domainInputType: 'manual',
      subdomain: '',
    },
  })

  const selectedDomain = useWatch({
    control: form.control,
    name: 'domain',
  })
  const isWildcardDomain = useMemo(
    () => selectedDomain && selectedDomain.includes('*.'),
    [selectedDomain]
  )

  const onSubmit = useCallback(
    async (data: CreateRouteFormData) => {
      let finalDomain = data.domain

      // If it's a wildcard domain, subdomain is required
      if (isWildcardDomain) {
        if (!data.subdomain || data.subdomain.trim() === '') {
          toast.error('Subdomain is required for wildcard domains')
          return
        }
        finalDomain = data.domain.replace('*.', `${data.subdomain}.`)
      }

      await createRouteMutation.mutateAsync({
        domain: finalDomain,
        host: data.host,
        port: data.port,
        domainInputType: data.domainInputType,
      })
    },
    [createRouteMutation, isWildcardDomain]
  )

  useEffect(() => {
    if (!isCreateDialogOpen) {
      form.reset({
        domain: '',
        host: '',
        port: 80,
        domainInputType:
          isLocalMode || !hasAvailableDomains ? 'manual' : 'select',
        subdomain: '',
      })
    }
  }, [isCreateDialogOpen, isLocalMode, hasAvailableDomains, form])

  useEffect(() => {
    if (editRoute) {
      editForm.reset({
        domain: editRoute.domain,
        host: editRoute.host,
        port: editRoute.port,
        domainInputType: 'manual',
        subdomain: '',
      })
    }
  }, [editRoute, editForm])

  const onEditSubmit = async (data: CreateRouteFormData) => {
    await updateRouteMutation.mutateAsync({
      domain: data.domain,
      host: data.host,
      port: data.port,
      domainInputType: 'manual',
    })
    editForm.reset()
  }

  const domainInputType = useWatch({
    control: form.control,
    name: 'domainInputType',
  })
  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">Routes Management</h2>
          <p className="text-sm text-muted-foreground">
            Configure custom domain routing and load balancing
          </p>
        </div>
        <Dialog open={isCreateDialogOpen} onOpenChange={setIsCreateDialogOpen}>
          <DialogTrigger asChild>
            <Button>
              <Plus className="mr-2 h-4 w-4" />
              Add Route
            </Button>
          </DialogTrigger>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Add New Route</DialogTitle>
            </DialogHeader>
            <Form {...form}>
              <form
                onSubmit={form.handleSubmit(onSubmit)}
                className="space-y-6"
              >
                {hasAvailableDomains && !isLocalMode && (
                  <FormField
                    control={form.control}
                    name="domainInputType"
                    render={({ field }) => (
                      <FormItem className="space-y-1">
                        <FormLabel>Domain Input Type</FormLabel>
                        <FormControl>
                          <RadioGroup
                            onValueChange={field.onChange}
                            value={field.value}
                            className="flex gap-4"
                          >
                            <div className="flex items-center space-x-2">
                              <RadioGroupItem value="select" id="select" />
                              <Label htmlFor="select">
                                Select Existing Domain
                              </Label>
                            </div>
                            <div className="flex items-center space-x-2">
                              <RadioGroupItem value="manual" id="manual" />
                              <Label htmlFor="manual">Enter Manually</Label>
                            </div>
                          </RadioGroup>
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                )}

                <FormField
                  control={form.control}
                  name="domain"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Domain</FormLabel>
                      <FormControl>
                        {domainInputType === 'select' &&
                        hasAvailableDomains &&
                        !isLocalMode ? (
                          <Select
                            onValueChange={field.onChange}
                            value={field.value}
                          >
                            <FormControl>
                              <SelectTrigger>
                                <SelectValue placeholder="Select a domain" />
                              </SelectTrigger>
                            </FormControl>
                            <SelectContent>
                              {domains?.domains?.map((domain) => (
                                <SelectItem
                                  key={domain.id}
                                  value={domain.domain}
                                >
                                  {domain.domain}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        ) : (
                          <Input placeholder="example.com" {...field} />
                        )}
                      </FormControl>
                      {(isLocalMode || !hasAvailableDomains) && (
                        <p className="text-sm text-muted-foreground">
                          {isLocalMode
                            ? 'Manual entry required in local development mode'
                            : 'No domains available - enter domain manually'}
                        </p>
                      )}
                      <FormMessage />
                    </FormItem>
                  )}
                />

                {isWildcardDomain && (
                  <FormField
                    control={form.control}
                    name="subdomain"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Subdomain</FormLabel>
                        <FormControl>
                          <div className="flex items-center gap-1">
                            <Input
                              placeholder="subdomain"
                              {...field}
                              className="flex-1"
                            />
                            <span className="text-sm text-muted-foreground">
                              {selectedDomain.replace('*', '')}
                            </span>
                          </div>
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                )}

                <FormField
                  control={form.control}
                  name="host"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Host</FormLabel>
                      <FormControl>
                        <Input
                          placeholder="localhost or IP address"
                          {...field}
                        />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="port"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Port</FormLabel>
                      <FormControl>
                        <Input
                          type="number"
                          placeholder="3000"
                          {...field}
                          onChange={(e) =>
                            field.onChange(Number(e.target.value))
                          }
                        />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                <div className="flex justify-end">
                  <Button
                    type="submit"
                    disabled={createRouteMutation.isPending}
                  >
                    {createRouteMutation.isPending
                      ? 'Creating...'
                      : 'Create Route'}
                  </Button>
                </div>
              </form>
            </Form>
          </DialogContent>
        </Dialog>
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
          description="Get started by adding a new route"
          action={
            <Button onClick={() => setIsCreateDialogOpen(true)}>
              <Plus className="mr-2 h-4 w-4" />
              Add Route
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
                  <div className="flex items-center gap-2">
                    <div>
                      <h3 className="truncate font-medium">{route.domain}</h3>
                      <p className="text-sm text-muted-foreground">
                        {route.host}:{route.port}
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
                        placeholder="3000"
                        {...field}
                        onChange={(e) => field.onChange(Number(e.target.value))}
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
