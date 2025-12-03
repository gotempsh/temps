'use client'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { EllipsisVertical, Loader2, Mail, Plus } from 'lucide-react'
import { AWSIcon } from '@/components/icons/AWSIcon'
import { ScalewayIcon } from '@/components/icons/ScalewayIcon'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

// Types for email providers (matching backend)
interface EmailProvider {
  id: number
  name: string
  provider_type: 'ses' | 'scaleway'
  region: string
  is_active: boolean
  credentials: Record<string, string>
  created_at: string
  updated_at: string
}

// Form schema
const createProviderSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  provider_type: z.enum(['ses', 'scaleway']),
  region: z.string().min(1, 'Region is required'),
  // SES credentials
  access_key_id: z.string().optional(),
  secret_access_key: z.string().optional(),
  // Scaleway credentials
  api_key: z.string().optional(),
  project_id: z.string().optional(),
}).refine((data) => {
  if (data.provider_type === 'ses') {
    return data.access_key_id && data.secret_access_key
  }
  if (data.provider_type === 'scaleway') {
    return data.api_key && data.project_id
  }
  return true
}, {
  message: 'Please provide the required credentials for the selected provider',
  path: ['access_key_id'],
})

type CreateProviderFormData = z.infer<typeof createProviderSchema>

// API functions using fetch directly until we regenerate API client
async function listEmailProviders(): Promise<EmailProvider[]> {
  const response = await fetch('/api/email-providers')
  if (!response.ok) {
    throw new Error('Failed to fetch email providers')
  }
  return response.json()
}

async function createEmailProvider(data: CreateProviderFormData): Promise<EmailProvider> {
  const body: Record<string, unknown> = {
    name: data.name,
    provider_type: data.provider_type,
    region: data.region,
  }

  if (data.provider_type === 'ses') {
    body.ses_credentials = {
      access_key_id: data.access_key_id,
      secret_access_key: data.secret_access_key,
    }
  } else if (data.provider_type === 'scaleway') {
    body.scaleway_credentials = {
      api_key: data.api_key,
      project_id: data.project_id,
    }
  }

  const response = await fetch('/api/email-providers', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })

  if (!response.ok) {
    const error = await response.json()
    throw new Error(error.detail || 'Failed to create email provider')
  }

  return response.json()
}

async function deleteEmailProvider(id: number): Promise<void> {
  const response = await fetch(`/api/email-providers/${id}`, {
    method: 'DELETE',
  })

  if (!response.ok) {
    const error = await response.json()
    throw new Error(error.detail || 'Failed to delete email provider')
  }
}

// AWS regions for SES
const awsRegions = [
  { value: 'us-east-1', label: 'US East (N. Virginia)' },
  { value: 'us-east-2', label: 'US East (Ohio)' },
  { value: 'us-west-1', label: 'US West (N. California)' },
  { value: 'us-west-2', label: 'US West (Oregon)' },
  { value: 'eu-west-1', label: 'Europe (Ireland)' },
  { value: 'eu-west-2', label: 'Europe (London)' },
  { value: 'eu-west-3', label: 'Europe (Paris)' },
  { value: 'eu-central-1', label: 'Europe (Frankfurt)' },
  { value: 'ap-southeast-1', label: 'Asia Pacific (Singapore)' },
  { value: 'ap-southeast-2', label: 'Asia Pacific (Sydney)' },
  { value: 'ap-northeast-1', label: 'Asia Pacific (Tokyo)' },
  { value: 'ap-south-1', label: 'Asia Pacific (Mumbai)' },
  { value: 'sa-east-1', label: 'South America (São Paulo)' },
]

// Scaleway regions
const scalewayRegions = [
  { value: 'fr-par', label: 'Paris, France' },
  { value: 'nl-ams', label: 'Amsterdam, Netherlands' },
  { value: 'pl-waw', label: 'Warsaw, Poland' },
]

function ProviderIcon({ type }: { type: 'ses' | 'scaleway' }) {
  if (type === 'ses') {
    return <AWSIcon className="h-5 w-5 text-[#FF9900]" />
  }
  return <ScalewayIcon className="h-5 w-5 text-[#4F0599]" />
}

function ProviderCard({
  provider,
  onDelete,
}: {
  provider: EmailProvider
  onDelete: (id: number) => void
}) {
  const [isDeleting, setIsDeleting] = useState(false)

  const handleDelete = async () => {
    setIsDeleting(true)
    try {
      await onDelete(provider.id)
    } finally {
      setIsDeleting(false)
    }
  }

  return (
    <Card>
      <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
        <div className="flex items-center gap-3">
          <ProviderIcon type={provider.provider_type} />
          <div>
            <CardTitle className="text-base font-medium leading-none">
              {provider.name}
            </CardTitle>
            <p className="text-xs text-muted-foreground mt-1 capitalize">
              {provider.provider_type === 'ses' ? 'AWS SES' : 'Scaleway'}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Badge variant={provider.is_active ? 'default' : 'secondary'}>
            {provider.is_active ? 'Active' : 'Inactive'}
          </Badge>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon" className="h-8 w-8">
                <EllipsisVertical className="h-4 w-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem disabled>Edit</DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                className="text-destructive"
                onClick={handleDelete}
                disabled={isDeleting}
              >
                {isDeleting ? 'Deleting...' : 'Delete'}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </CardHeader>
      <CardContent>
        <div className="space-y-2 text-sm">
          <div className="flex justify-between">
            <span className="text-muted-foreground">Region</span>
            <span className="font-mono">{provider.region}</span>
          </div>
          <div className="flex justify-between">
            <span className="text-muted-foreground">Created</span>
            <span>
              {formatDistanceToNow(new Date(provider.created_at), { addSuffix: true })}
            </span>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}

function LoadingSkeleton() {
  return (
    <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
      {[1, 2, 3].map((i) => (
        <Card key={i}>
          <CardHeader className="pb-2">
            <div className="flex items-center gap-3">
              <Skeleton className="h-10 w-10 rounded-full" />
              <div className="space-y-2">
                <Skeleton className="h-4 w-24" />
                <Skeleton className="h-3 w-16" />
              </div>
            </div>
          </CardHeader>
          <CardContent>
            <div className="space-y-2">
              <Skeleton className="h-4 w-full" />
              <Skeleton className="h-4 w-3/4" />
            </div>
          </CardContent>
        </Card>
      ))}
    </div>
  )
}

export function EmailProvidersManagement() {
  const [isDialogOpen, setIsDialogOpen] = useState(false)
  const queryClient = useQueryClient()

  const { data: providers, isLoading } = useQuery({
    queryKey: ['email-providers'],
    queryFn: listEmailProviders,
  })

  const createMutation = useMutation({
    mutationFn: createEmailProvider,
    onSuccess: () => {
      toast.success('Email provider created successfully')
      queryClient.invalidateQueries({ queryKey: ['email-providers'] })
      setIsDialogOpen(false)
      form.reset()
    },
    onError: (error: Error) => {
      toast.error('Failed to create provider', {
        description: error.message,
      })
    },
  })

  const deleteMutation = useMutation({
    mutationFn: deleteEmailProvider,
    onSuccess: () => {
      toast.success('Email provider deleted successfully')
      queryClient.invalidateQueries({ queryKey: ['email-providers'] })
    },
    onError: (error: Error) => {
      toast.error('Failed to delete provider', {
        description: error.message,
      })
    },
  })

  const form = useForm<CreateProviderFormData>({
    resolver: zodResolver(createProviderSchema),
    defaultValues: {
      name: '',
      provider_type: 'ses',
      region: 'us-east-1',
      access_key_id: '',
      secret_access_key: '',
      api_key: '',
      project_id: '',
    },
  })

  const providerType = form.watch('provider_type')
  const regions = providerType === 'ses' ? awsRegions : scalewayRegions

  const onSubmit = (data: CreateProviderFormData) => {
    createMutation.mutate(data)
  }

  const handleDelete = (id: number) => {
    deleteMutation.mutate(id)
  }

  const hasProviders = providers && providers.length > 0

  return (
    <div className="space-y-4">
      <div className="flex justify-between items-center">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">Email Providers</h2>
          <p className="text-muted-foreground">
            Configure cloud email providers like AWS SES or Scaleway to send emails.
          </p>
        </div>

        {hasProviders && (
          <Button onClick={() => setIsDialogOpen(true)}>
            <Plus className="h-4 w-4 mr-2" />
            Add Provider
          </Button>
        )}
      </div>

      {isLoading ? (
        <LoadingSkeleton />
      ) : !hasProviders ? (
        <EmptyState
          icon={Mail}
          title="No email providers configured"
          description="Add your first email provider to start sending transactional emails from your applications."
          action={
            <Button onClick={() => setIsDialogOpen(true)}>
              <Plus className="h-4 w-4 mr-2" />
              Add Provider
            </Button>
          }
        />
      ) : (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {providers.map((provider) => (
            <ProviderCard
              key={provider.id}
              provider={provider}
              onDelete={handleDelete}
            />
          ))}
        </div>
      )}

      <Dialog open={isDialogOpen} onOpenChange={setIsDialogOpen}>
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>Add Email Provider</DialogTitle>
            <DialogDescription>
              Configure a cloud email provider to send transactional emails.
            </DialogDescription>
          </DialogHeader>

          <Form {...form}>
            <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
              <FormField
                control={form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input placeholder="My Email Provider" {...field} />
                    </FormControl>
                    <FormDescription>
                      A friendly name to identify this provider.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="provider_type"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Provider Type</FormLabel>
                    <Select
                      onValueChange={(value) => {
                        field.onChange(value)
                        // Reset region when provider changes
                        form.setValue('region', value === 'ses' ? 'us-east-1' : 'fr-par')
                      }}
                      value={field.value}
                    >
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue placeholder="Select a provider" />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value="ses">
                          <div className="flex items-center gap-2">
                            <AWSIcon className="h-4 w-4 text-[#FF9900]" />
                            AWS SES
                          </div>
                        </SelectItem>
                        <SelectItem value="scaleway">
                          <div className="flex items-center gap-2">
                            <ScalewayIcon className="h-4 w-4 text-[#4F0599]" />
                            Scaleway
                          </div>
                        </SelectItem>
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="region"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Region</FormLabel>
                    <Select onValueChange={field.onChange} value={field.value}>
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue placeholder="Select a region" />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {regions.map((region) => (
                          <SelectItem key={region.value} value={region.value}>
                            <div className="flex items-center justify-between gap-4 w-full">
                              <span>{region.label}</span>
                              <span className="font-mono text-xs text-muted-foreground">
                                {region.value}
                              </span>
                            </div>
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />

              {providerType === 'ses' && (
                <>
                  <FormField
                    control={form.control}
                    name="access_key_id"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Access Key ID</FormLabel>
                        <FormControl>
                          <Input placeholder="AKIAIOSFODNN7EXAMPLE" {...field} />
                        </FormControl>
                        <FormDescription>
                          Your AWS access key ID with SES permissions.
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={form.control}
                    name="secret_access_key"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Secret Access Key</FormLabel>
                        <FormControl>
                          <Input
                            type="password"
                            placeholder="wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          Your AWS secret access key.
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </>
              )}

              {providerType === 'scaleway' && (
                <>
                  <FormField
                    control={form.control}
                    name="api_key"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>API Key</FormLabel>
                        <FormControl>
                          <Input
                            type="password"
                            placeholder="scw-secret-key-12345"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          Your Scaleway secret key with Transactional Email permissions.
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={form.control}
                    name="project_id"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Project ID</FormLabel>
                        <FormControl>
                          <Input
                            placeholder="12345678-1234-1234-1234-123456789012"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          Your Scaleway project ID.
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </>
              )}

              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => setIsDialogOpen(false)}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={createMutation.isPending}>
                  {createMutation.isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Add Provider
                </Button>
              </DialogFooter>
            </form>
          </Form>
        </DialogContent>
      </Dialog>
    </div>
  )
}
