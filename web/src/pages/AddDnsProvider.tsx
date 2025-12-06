import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { AlertCircle, ArrowLeft, Cloud, Globe, Loader2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'

// Types based on the backend API
type DnsProviderType = 'cloudflare' | 'namecheap'

interface DnsProviderResponse {
  id: number
  name: string
  provider_type: string
  credentials: Record<string, unknown>
  is_active: boolean
  description: string | null
  last_used_at: string | null
  last_error: string | null
  created_at: string
  updated_at: string
}

interface CreateDnsProviderRequest {
  name: string
  provider_type: DnsProviderType
  credentials: Record<string, unknown>
  description?: string
}

// API function using fetch
async function createDnsProvider(
  request: CreateDnsProviderRequest
): Promise<DnsProviderResponse> {
  const response = await fetch('/dns-providers', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    credentials: 'include',
    body: JSON.stringify(request),
  })
  if (!response.ok) {
    const error = await response.json().catch(() => ({}))
    throw new Error(error.detail || 'Failed to create DNS provider')
  }
  return response.json()
}

// Cloudflare form schema
const cloudflareFormSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  description: z.string().optional(),
  api_token: z.string().min(1, 'API token is required'),
  account_id: z.string().optional(),
})

type CloudflareFormData = z.infer<typeof cloudflareFormSchema>

// Namecheap form schema
const namecheapFormSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  description: z.string().optional(),
  api_user: z.string().min(1, 'API user is required'),
  api_key: z.string().min(1, 'API key is required'),
  client_ip: z.string().optional(),
  sandbox: z.boolean(),
})

type NamecheapFormData = z.infer<typeof namecheapFormSchema>

// Provider card component for selection
function ProviderCard({
  name,
  icon: Icon,
  description,
  selected,
  onClick,
}: {
  name: string
  icon: React.ElementType
  description: string
  selected: boolean
  onClick: () => void
}) {
  return (
    <div
      className={`cursor-pointer rounded-lg border-2 p-4 transition-all hover:border-primary/50 ${
        selected ? 'border-primary bg-primary/5' : 'border-muted'
      }`}
      onClick={onClick}
    >
      <div className="flex items-center gap-3">
        <div
          className={`flex h-10 w-10 items-center justify-center rounded-lg ${
            selected ? 'bg-primary text-primary-foreground' : 'bg-muted'
          }`}
        >
          <Icon className="h-5 w-5" />
        </div>
        <div>
          <h3 className="font-medium">{name}</h3>
          <p className="text-sm text-muted-foreground">{description}</p>
        </div>
      </div>
    </div>
  )
}

export function AddDnsProvider() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [providerType, setProviderType] = useState<DnsProviderType>('cloudflare')
  const [error, setError] = useState<string | null>(null)

  const cloudflareForm = useForm<CloudflareFormData>({
    resolver: zodResolver(cloudflareFormSchema),
    defaultValues: {
      name: '',
      description: '',
      api_token: '',
      account_id: '',
    },
  })

  const namecheapForm = useForm<NamecheapFormData>({
    resolver: zodResolver(namecheapFormSchema),
    defaultValues: {
      name: '',
      description: '',
      api_user: '',
      api_key: '',
      client_ip: '',
      sandbox: false,
    },
  })

  const createProviderMut = useMutation({
    mutationFn: createDnsProvider,
    onSuccess: (provider) => {
      toast.success('DNS provider created successfully')
      queryClient.invalidateQueries({ queryKey: ['dnsProviders'] })
      navigate(`/dns-providers/${provider.id}`)
    },
    onError: (err: Error) => {
      setError(err.message)
      toast.error('Failed to create DNS provider', {
        description: err.message,
      })
    },
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'DNS Providers', href: '/dns-providers' },
      { label: 'Add Provider' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Add DNS Provider')

  const handleProviderTypeChange = (type: DnsProviderType) => {
    setProviderType(type)
    setError(null)
  }

  const onCloudflareSubmit = (data: CloudflareFormData) => {
    setError(null)
    const request: CreateDnsProviderRequest = {
      name: data.name,
      provider_type: 'cloudflare',
      description: data.description || undefined,
      credentials: {
        type: 'cloudflare',
        api_token: data.api_token,
        account_id: data.account_id || undefined,
      },
    }
    createProviderMut.mutate(request)
  }

  const onNamecheapSubmit = (data: NamecheapFormData) => {
    setError(null)
    const request: CreateDnsProviderRequest = {
      name: data.name,
      provider_type: 'namecheap',
      description: data.description || undefined,
      credentials: {
        type: 'namecheap',
        api_user: data.api_user,
        api_key: data.api_key,
        client_ip: data.client_ip || undefined,
        sandbox: data.sandbox,
      },
    }
    createProviderMut.mutate(request)
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6 p-6 max-w-3xl mx-auto">
        <div className="flex items-center gap-4">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate('/dns-providers')}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h1 className="text-2xl font-bold">Add DNS Provider</h1>
            <p className="text-muted-foreground">
              Connect a DNS provider for automatic DNS record management
            </p>
          </div>
        </div>

        {/* Provider Selection */}
        <Card>
          <CardHeader>
            <CardTitle>Select Provider</CardTitle>
            <CardDescription>
              Choose your DNS provider to get started
            </CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4 sm:grid-cols-2">
            <ProviderCard
              name="Cloudflare"
              icon={Cloud}
              description="Global CDN & DNS provider"
              selected={providerType === 'cloudflare'}
              onClick={() => handleProviderTypeChange('cloudflare')}
            />
            <ProviderCard
              name="Namecheap"
              icon={Globe}
              description="Domain registrar & DNS"
              selected={providerType === 'namecheap'}
              onClick={() => handleProviderTypeChange('namecheap')}
            />
          </CardContent>
        </Card>

        {/* Cloudflare Form */}
        {providerType === 'cloudflare' && (
          <Form {...cloudflareForm}>
            <form
              onSubmit={cloudflareForm.handleSubmit(onCloudflareSubmit)}
              className="space-y-6"
            >
              <Card>
                <CardHeader>
                  <CardTitle>Basic Information</CardTitle>
                  <CardDescription>
                    Give your provider a name to identify it
                  </CardDescription>
                </CardHeader>
                <CardContent className="space-y-4">
                  <FormField
                    control={cloudflareForm.control}
                    name="name"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Name</FormLabel>
                        <FormControl>
                          <Input
                            placeholder="My Cloudflare Account"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          A friendly name to identify this provider
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={cloudflareForm.control}
                    name="description"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Description (optional)</FormLabel>
                        <FormControl>
                          <Textarea
                            placeholder="Main DNS provider for production domains"
                            {...field}
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </CardContent>
              </Card>

              <Card>
                <CardHeader>
                  <CardTitle>Credentials</CardTitle>
                  <CardDescription>
                    Enter your Cloudflare API token. You can create one at
                    dash.cloudflare.com/profile/api-tokens
                  </CardDescription>
                </CardHeader>
                <CardContent className="space-y-4">
                  <FormField
                    control={cloudflareForm.control}
                    name="api_token"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>API Token</FormLabel>
                        <FormControl>
                          <Input
                            type="password"
                            placeholder="Enter your Cloudflare API token"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          Create a token with Zone:Read and DNS:Edit permissions
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={cloudflareForm.control}
                    name="account_id"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Account ID (optional)</FormLabel>
                        <FormControl>
                          <Input
                            placeholder="Enter your Cloudflare account ID"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          Optional. Found in your Cloudflare dashboard URL
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </CardContent>
              </Card>

              {error && (
                <Alert variant="destructive">
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>{error}</AlertDescription>
                </Alert>
              )}

              <div className="flex justify-end gap-4">
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => navigate('/dns-providers')}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={createProviderMut.isPending}>
                  {createProviderMut.isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Create Provider
                </Button>
              </div>
            </form>
          </Form>
        )}

        {/* Namecheap Form */}
        {providerType === 'namecheap' && (
          <Form {...namecheapForm}>
            <form
              onSubmit={namecheapForm.handleSubmit(onNamecheapSubmit)}
              className="space-y-6"
            >
              <Card>
                <CardHeader>
                  <CardTitle>Basic Information</CardTitle>
                  <CardDescription>
                    Give your provider a name to identify it
                  </CardDescription>
                </CardHeader>
                <CardContent className="space-y-4">
                  <FormField
                    control={namecheapForm.control}
                    name="name"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Name</FormLabel>
                        <FormControl>
                          <Input
                            placeholder="My Namecheap Account"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          A friendly name to identify this provider
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={namecheapForm.control}
                    name="description"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Description (optional)</FormLabel>
                        <FormControl>
                          <Textarea
                            placeholder="Domain registrar for company domains"
                            {...field}
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </CardContent>
              </Card>

              <Card>
                <CardHeader>
                  <CardTitle>Credentials</CardTitle>
                  <CardDescription>
                    Enter your Namecheap API credentials
                  </CardDescription>
                </CardHeader>
                <CardContent className="space-y-4">
                  <FormField
                    control={namecheapForm.control}
                    name="api_user"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>API User</FormLabel>
                        <FormControl>
                          <Input
                            placeholder="Your Namecheap username"
                            {...field}
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={namecheapForm.control}
                    name="api_key"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>API Key</FormLabel>
                        <FormControl>
                          <Input
                            type="password"
                            placeholder="Your Namecheap API key"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          Enable API access in your Namecheap profile settings
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={namecheapForm.control}
                    name="client_ip"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Client IP (optional)</FormLabel>
                        <FormControl>
                          <Input
                            placeholder="Your whitelisted IP address"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          The IP address whitelisted for API access
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={namecheapForm.control}
                    name="sandbox"
                    render={({ field }) => (
                      <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                        <div className="space-y-0.5">
                          <FormLabel className="text-base">
                            Sandbox Mode
                          </FormLabel>
                          <FormDescription>
                            Use Namecheap sandbox environment for testing
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
                </CardContent>
              </Card>

              {error && (
                <Alert variant="destructive">
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>{error}</AlertDescription>
                </Alert>
              )}

              <div className="flex justify-end gap-4">
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => navigate('/dns-providers')}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={createProviderMut.isPending}>
                  {createProviderMut.isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Create Provider
                </Button>
              </div>
            </form>
          </Form>
        )}
      </div>
    </div>
  )
}
