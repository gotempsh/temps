import {
  addManagedDomain,
  deleteProvider,
  getProvider,
  listManagedDomains,
  listProviderZones,
  removeManagedDomain,
  testProviderConnection,
  updateProvider,
  verifyManagedDomain,
  type ManagedDomainResponse,
  type UpdateDnsProviderRequest,
} from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
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
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  CheckCircle2,
  Cloud,
  Edit,
  Globe,
  Loader2,
  Plus,
  RefreshCw,
  TestTube2,
  Trash2,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'

// Helper function to get provider icon
function getProviderIcon(providerType: string) {
  switch (providerType.toLowerCase()) {
    case 'cloudflare':
      return <Cloud className="h-5 w-5 text-orange-500" />
    default:
      return <Globe className="h-5 w-5" />
  }
}

// Helper function to format provider type for display
function formatProviderType(type: string): string {
  switch (type.toLowerCase()) {
    case 'cloudflare':
      return 'Cloudflare'
    case 'namecheap':
      return 'Namecheap'
    default:
      return type.charAt(0).toUpperCase() + type.slice(1)
  }
}

// Edit form schema
const editFormSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  description: z.string().optional(),
  is_active: z.boolean(),
})

type EditFormData = z.infer<typeof editFormSchema>

// Add domain form schema
const addDomainFormSchema = z.object({
  domain: z
    .string()
    .min(1, 'Domain is required')
    .regex(
      /^([a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$/,
      'Invalid domain format'
    ),
  auto_manage: z.boolean(),
})

type AddDomainFormData = z.infer<typeof addDomainFormSchema>

export default function DnsProviderDetail() {
  const { id } = useParams<{ id: string }>()
  const providerId = parseInt(id || '0', 10)
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)
  const [isDeleteDialogOpen, setIsDeleteDialogOpen] = useState(false)
  const [isAddDomainDialogOpen, setIsAddDomainDialogOpen] = useState(false)
  const [domainToRemove, setDomainToRemove] =
    useState<ManagedDomainResponse | null>(null)

  // Queries
  const {
    data: provider,
    isLoading,
    error,
    refetch,
  } = useQuery({
    queryKey: ['dnsProvider', providerId],
    queryFn: async () => {
      const response = await getProvider({ path: { id: providerId } })
      return response.data
    },
    enabled: !!providerId,
  })

  const { data: managedDomains, refetch: refetchDomains } = useQuery({
    queryKey: ['dnsProviderDomains', providerId],
    queryFn: async () => {
      const response = await listManagedDomains({ path: { id: providerId } })
      return response.data
    },
    enabled: !!providerId,
  })

  const { data: zones } = useQuery({
    queryKey: ['dnsProviderZones', providerId],
    queryFn: async () => {
      const response = await listProviderZones({ path: { id: providerId } })
      return response.data
    },
    enabled: !!providerId && !!provider?.is_active,
  })

  // Mutations
  const updateProviderMut = useMutation({
    mutationFn: async (data: Partial<EditFormData>) => {
      const body: UpdateDnsProviderRequest = {
        name: data.name,
        description: data.description,
        is_active: data.is_active,
      }
      const response = await updateProvider({ path: { id: providerId }, body })
      return response.data
    },
    onSuccess: () => {
      toast.success('Provider updated successfully')
      queryClient.invalidateQueries({ queryKey: ['dnsProvider', providerId] })
      queryClient.invalidateQueries({ queryKey: ['dnsProviders'] })
      setIsEditDialogOpen(false)
    },
    onError: (err: Error) => {
      toast.error('Failed to update provider', {
        description: err.message,
      })
    },
  })

  const deleteProviderMut = useMutation({
    mutationFn: () => deleteProvider({ path: { id: providerId } }),
    onSuccess: () => {
      toast.success('Provider deleted successfully')
      queryClient.invalidateQueries({ queryKey: ['dnsProviders'] })
      navigate('/dns-providers')
    },
    onError: (err: Error) => {
      toast.error('Failed to delete provider', {
        description: err.message,
      })
    },
  })

  const testConnectionMut = useMutation({
    mutationFn: async () => {
      const response = await testProviderConnection({ path: { id: providerId } })
      return response.data
    },
    onSuccess: (result) => {
      if (result?.success) {
        toast.success('Connection test successful', {
          description: result.message,
        })
      } else {
        toast.error('Connection test failed', {
          description: result?.message,
        })
      }
      refetch()
    },
    onError: (err: Error) => {
      toast.error('Connection test failed', {
        description: err.message,
      })
    },
  })

  const addDomainMut = useMutation({
    mutationFn: async (data: AddDomainFormData) => {
      const response = await addManagedDomain({
        path: { id: providerId },
        body: { domain: data.domain, auto_manage: data.auto_manage },
      })
      return response.data
    },
    onSuccess: () => {
      toast.success('Domain added successfully')
      refetchDomains()
      setIsAddDomainDialogOpen(false)
      addDomainForm.reset()
    },
    onError: (err: Error) => {
      toast.error('Failed to add domain', {
        description: err.message,
      })
    },
  })

  const removeDomainMut = useMutation({
    mutationFn: (domain: string) =>
      removeManagedDomain({
        path: { provider_id: providerId, domain },
      }),
    onSuccess: () => {
      toast.success('Domain removed successfully')
      refetchDomains()
      setDomainToRemove(null)
    },
    onError: (err: Error) => {
      toast.error('Failed to remove domain', {
        description: err.message,
      })
    },
  })

  const verifyDomainMut = useMutation({
    mutationFn: (domain: string) =>
      verifyManagedDomain({
        path: { provider_id: providerId, domain },
      }),
    onSuccess: () => {
      toast.success('Domain verified successfully')
      refetchDomains()
    },
    onError: (err: Error) => {
      toast.error('Failed to verify domain', {
        description: err.message,
      })
    },
  })

  // Forms
  const editForm = useForm<EditFormData>({
    resolver: zodResolver(editFormSchema),
    defaultValues: {
      name: provider?.name || '',
      description: provider?.description || '',
      is_active: provider?.is_active ?? true,
    },
  })

  const addDomainForm = useForm<AddDomainFormData>({
    resolver: zodResolver(addDomainFormSchema),
    defaultValues: {
      domain: '',
      auto_manage: true,
    },
  })

  // Update form values when provider loads
  useEffect(() => {
    if (provider) {
      editForm.reset({
        name: provider.name,
        description: provider.description || '',
        is_active: provider.is_active,
      })
    }
  }, [provider, editForm])

  useEffect(() => {
    if (provider) {
      setBreadcrumbs([
        { label: 'DNS Providers', href: '/dns-providers' },
        { label: provider.name },
      ])
    }
  }, [provider, setBreadcrumbs])

  usePageTitle(provider?.name || 'DNS Provider')

  if (isLoading) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="space-y-6 p-6">
          <div className="flex items-center gap-4">
            <Skeleton className="h-10 w-10 rounded-full" />
            <div className="space-y-2">
              <Skeleton className="h-6 w-48" />
              <Skeleton className="h-4 w-32" />
            </div>
          </div>
          <Card>
            <CardContent className="p-6">
              <div className="space-y-4">
                <Skeleton className="h-4 w-full" />
                <Skeleton className="h-4 w-3/4" />
                <Skeleton className="h-4 w-1/2" />
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    )
  }

  if (error || !provider) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="space-y-6 p-6">
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Error</AlertTitle>
            <AlertDescription>
              Failed to load DNS provider. The provider may have been deleted or
              you may not have permission to view it.
            </AlertDescription>
          </Alert>
          <Button onClick={() => navigate('/dns-providers')}>
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to Providers
          </Button>
        </div>
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6 p-6">
        {/* Header */}
        <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-4">
            <Button
              variant="ghost"
              size="icon"
              onClick={() => navigate('/dns-providers')}
            >
              <ArrowLeft className="h-4 w-4" />
            </Button>
            <div className="flex items-center gap-3">
              {getProviderIcon(provider.provider_type)}
              <div>
                <h1 className="text-2xl font-bold">{provider.name}</h1>
                <div className="flex items-center gap-2 text-sm text-muted-foreground">
                  <span>{formatProviderType(provider.provider_type)}</span>
                  <span>•</span>
                  <span>
                    Created <TimeAgo date={provider.created_at} />
                  </span>
                </div>
              </div>
            </div>
          </div>
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => testConnectionMut.mutate()}
              disabled={testConnectionMut.isPending}
            >
              {testConnectionMut.isPending ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : (
                <TestTube2 className="mr-2 h-4 w-4" />
              )}
              Test Connection
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setIsEditDialogOpen(true)}
            >
              <Edit className="mr-2 h-4 w-4" />
              Edit
            </Button>
            <Button
              variant="destructive"
              size="sm"
              onClick={() => setIsDeleteDialogOpen(true)}
            >
              <Trash2 className="mr-2 h-4 w-4" />
              Delete
            </Button>
          </div>
        </div>

        {/* Status */}
        <div className="flex items-center gap-4">
          {provider.is_active ? (
            <Badge variant="secondary" className="flex items-center gap-1">
              <CheckCircle2 className="h-3 w-3" />
              Active
            </Badge>
          ) : (
            <Badge variant="destructive" className="flex items-center gap-1">
              <XCircle className="h-3 w-3" />
              Inactive
            </Badge>
          )}
          {provider.last_error && (
            <Badge variant="outline" className="flex items-center gap-1 text-destructive">
              <AlertCircle className="h-3 w-3" />
              {provider.last_error}
            </Badge>
          )}
        </div>

        {/* Description */}
        {provider.description && (
          <p className="text-muted-foreground">{provider.description}</p>
        )}

        <Separator />

        {/* Credentials (masked) */}
        <Card>
          <CardHeader>
            <CardTitle>Credentials</CardTitle>
            <CardDescription>
              Stored credentials for this provider (masked for security)
            </CardDescription>
          </CardHeader>
          <CardContent>
            <div className="grid gap-4 sm:grid-cols-2">
              {Object.entries(
                provider.credentials as Record<string, unknown>
              ).map(([key, value]) => (
                <div key={key} className="space-y-1">
                  <p className="text-sm font-medium">{key}</p>
                  <p className="text-sm text-muted-foreground font-mono">
                    {String(value)}
                  </p>
                </div>
              ))}
            </div>
          </CardContent>
        </Card>

        {/* Zones */}
        {zones && zones.zones.length > 0 && (
          <Card>
            <CardHeader>
              <CardTitle>Available Zones</CardTitle>
              <CardDescription>
                DNS zones available in this provider account
              </CardDescription>
            </CardHeader>
            <CardContent>
              <div className="grid gap-2">
                {zones.zones.map((zone) => (
                  <div
                    key={zone.id}
                    className="flex items-center justify-between p-3 border rounded-lg"
                  >
                    <div>
                      <p className="font-medium">{zone.name}</p>
                      <p className="text-sm text-muted-foreground">
                        ID: {zone.id}
                      </p>
                    </div>
                    <Badge variant="outline">{zone.status}</Badge>
                  </div>
                ))}
              </div>
            </CardContent>
          </Card>
        )}

        {/* Managed Domains */}
        <Card>
          <CardHeader className="flex flex-row items-center justify-between">
            <div>
              <CardTitle>Managed Domains</CardTitle>
              <CardDescription>
                Domains managed by this DNS provider
              </CardDescription>
            </div>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={() => refetchDomains()}
              >
                <RefreshCw className="h-4 w-4" />
              </Button>
              <Button
                size="sm"
                onClick={() => setIsAddDomainDialogOpen(true)}
              >
                <Plus className="mr-2 h-4 w-4" />
                Add Domain
              </Button>
            </div>
          </CardHeader>
          <CardContent>
            {!managedDomains?.length ? (
              <div className="text-center py-8 text-muted-foreground">
                <Globe className="h-12 w-12 mx-auto mb-4 opacity-50" />
                <p>No managed domains yet</p>
                <p className="text-sm">
                  Add a domain to start managing its DNS records
                </p>
              </div>
            ) : (
              <div className="grid gap-3">
                {managedDomains.map((domain) => (
                  <div
                    key={domain.id}
                    className="flex items-center justify-between p-4 border rounded-lg"
                  >
                    <div className="space-y-1">
                      <div className="flex items-center gap-2">
                        <p className="font-medium">{domain.domain}</p>
                        {domain.verified ? (
                          <Badge
                            variant="secondary"
                            className="flex items-center gap-1"
                          >
                            <CheckCircle2 className="h-3 w-3" />
                            Verified
                          </Badge>
                        ) : (
                          <Badge
                            variant="outline"
                            className="flex items-center gap-1"
                          >
                            <XCircle className="h-3 w-3" />
                            Not Verified
                          </Badge>
                        )}
                        {domain.auto_manage && (
                          <Badge variant="outline">Auto-managed</Badge>
                        )}
                      </div>
                      {domain.zone_id && (
                        <p className="text-sm text-muted-foreground">
                          Zone ID: {domain.zone_id}
                        </p>
                      )}
                      {domain.verification_error && (
                        <p className="text-sm text-destructive">
                          {domain.verification_error}
                        </p>
                      )}
                    </div>
                    <div className="flex items-center gap-2">
                      {!domain.verified && (
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() =>
                            verifyDomainMut.mutate(domain.domain)
                          }
                          disabled={verifyDomainMut.isPending}
                        >
                          {verifyDomainMut.isPending ? (
                            <Loader2 className="h-4 w-4 animate-spin" />
                          ) : (
                            'Verify'
                          )}
                        </Button>
                      )}
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => setDomainToRemove(domain)}
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      {/* Edit Dialog */}
      <Dialog open={isEditDialogOpen} onOpenChange={setIsEditDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Edit DNS Provider</DialogTitle>
            <DialogDescription>
              Update the provider settings
            </DialogDescription>
          </DialogHeader>
          <Form {...editForm}>
            <form
              onSubmit={editForm.handleSubmit((data) =>
                updateProviderMut.mutate(data)
              )}
              className="space-y-4"
            >
              <FormField
                control={editForm.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={editForm.control}
                name="description"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Description</FormLabel>
                    <FormControl>
                      <Textarea {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={editForm.control}
                name="is_active"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                    <div className="space-y-0.5">
                      <FormLabel className="text-base">Active</FormLabel>
                      <FormDescription>
                        Enable or disable this provider
                      </FormDescription>
                    </div>
                    <FormControl>
                      <Switch
                        checked={field.value}
                        onCheckedChange={field.onChange}
                      />
                    </FormControl>
                  </FormItem>
                )}
              />

              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => setIsEditDialogOpen(false)}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={updateProviderMut.isPending}>
                  {updateProviderMut.isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Save Changes
                </Button>
              </DialogFooter>
            </form>
          </Form>
        </DialogContent>
      </Dialog>

      {/* Add Domain Dialog */}
      <Dialog
        open={isAddDomainDialogOpen}
        onOpenChange={setIsAddDomainDialogOpen}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Add Managed Domain</DialogTitle>
            <DialogDescription>
              Add a domain to be managed by this DNS provider
            </DialogDescription>
          </DialogHeader>
          <Form {...addDomainForm}>
            <form
              onSubmit={addDomainForm.handleSubmit((data) =>
                addDomainMut.mutate(data)
              )}
              className="space-y-4"
            >
              <FormField
                control={addDomainForm.control}
                name="domain"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Domain</FormLabel>
                    <FormControl>
                      <Input placeholder="example.com" {...field} />
                    </FormControl>
                    <FormDescription>
                      Enter the domain name (e.g., example.com)
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={addDomainForm.control}
                name="auto_manage"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                    <div className="space-y-0.5">
                      <FormLabel className="text-base">
                        Auto-manage DNS
                      </FormLabel>
                      <FormDescription>
                        Automatically create and update DNS records
                      </FormDescription>
                    </div>
                    <FormControl>
                      <Switch
                        checked={field.value}
                        onCheckedChange={field.onChange}
                      />
                    </FormControl>
                  </FormItem>
                )}
              />

              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => setIsAddDomainDialogOpen(false)}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={addDomainMut.isPending}>
                  {addDomainMut.isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Add Domain
                </Button>
              </DialogFooter>
            </form>
          </Form>
        </DialogContent>
      </Dialog>

      {/* Delete Provider Dialog */}
      <AlertDialog
        open={isDeleteDialogOpen}
        onOpenChange={setIsDeleteDialogOpen}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete DNS Provider</AlertDialogTitle>
            <AlertDialogDescription>
              Are you sure you want to delete &quot;{provider.name}&quot;? This
              action cannot be undone and will remove all managed domains
              associated with this provider.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={deleteProviderMut.isPending}
              onClick={() => deleteProviderMut.mutate()}
            >
              {deleteProviderMut.isPending ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Deleting...
                </>
              ) : (
                'Delete Provider'
              )}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Remove Domain Dialog */}
      <AlertDialog
        open={!!domainToRemove}
        onOpenChange={(open) => !open && setDomainToRemove(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Remove Managed Domain</AlertDialogTitle>
            <AlertDialogDescription>
              Are you sure you want to remove &quot;{domainToRemove?.domain}
              &quot; from this provider? DNS records will no longer be
              automatically managed.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={removeDomainMut.isPending}
              onClick={() =>
                domainToRemove && removeDomainMut.mutate(domainToRemove.domain)
              }
            >
              {removeDomainMut.isPending ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Removing...
                </>
              ) : (
                'Remove Domain'
              )}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
