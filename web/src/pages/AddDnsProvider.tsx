import {
  createDnsProvider as createProvider,
  type CreateDnsProviderRequest,
} from '@/api/client'
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
import { cn } from '@/lib/utils'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  ArrowRight,
  Check,
  Loader2,
  Search,
} from 'lucide-react'
import { useEffect, useMemo, useState, type ComponentType, type SVGProps } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import { useEnterSubmit } from '@/hooks/useEnterSubmit'
import {
  AwsRoute53Icon,
  AzureIcon,
  CloudflareIcon,
  DigitalOceanIcon,
  GoogleCloudIcon,
  NamecheapIcon,
} from '@/components/icons/DnsProviderIcons'

type DnsProviderType =
  | 'cloudflare'
  | 'namecheap'
  | 'route53'
  | 'digitalocean'
  | 'gcp'
  | 'azure'

// Extended credentials type until API client is regenerated
type ExtendedDnsProviderCredentials =
  | { type: 'cloudflare'; api_token: string; account_id?: string | null }
  | {
      type: 'namecheap'
      api_user: string
      api_key: string
      client_ip?: string | null
      sandbox?: boolean
    }
  | {
      type: 'route53'
      access_key_id: string
      secret_access_key: string
      session_token?: string | null
      region?: string | null
    }
  | { type: 'digitalocean'; api_token: string }
  | {
      type: 'gcp'
      service_account_email: string
      private_key: string
      project_id: string
    }
  | {
      type: 'azure'
      tenant_id: string
      client_id: string
      client_secret: string
      subscription_id: string
      resource_group: string
    }

// Provider info for the selection step
interface ProviderInfo {
  type: DnsProviderType
  name: string
  description: string
  icon: ComponentType<SVGProps<SVGSVGElement> & { className?: string }>
  keywords: string[]
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

// Route53 form schema
const route53FormSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  description: z.string().optional(),
  access_key_id: z.string().min(1, 'Access Key ID is required'),
  secret_access_key: z.string().min(1, 'Secret Access Key is required'),
  session_token: z.string().optional(),
  region: z.string().optional(),
})

type Route53FormData = z.infer<typeof route53FormSchema>

// DigitalOcean form schema
const digitaloceanFormSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  description: z.string().optional(),
  api_token: z.string().min(1, 'API token is required'),
})

type DigitalOceanFormData = z.infer<typeof digitaloceanFormSchema>

// GCP form schema
const gcpFormSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  description: z.string().optional(),
  service_account_email: z.string().email('Valid email is required'),
  private_key: z.string().min(1, 'Private key is required'),
  project_id: z.string().min(1, 'Project ID is required'),
})

type GcpFormData = z.infer<typeof gcpFormSchema>

// Azure form schema
const azureFormSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  description: z.string().optional(),
  tenant_id: z.string().min(1, 'Tenant ID is required'),
  client_id: z.string().min(1, 'Client ID is required'),
  client_secret: z.string().min(1, 'Client Secret is required'),
  subscription_id: z.string().min(1, 'Subscription ID is required'),
  resource_group: z.string().min(1, 'Resource Group is required'),
})

type AzureFormData = z.infer<typeof azureFormSchema>

// Provider data
const PROVIDERS: ProviderInfo[] = [
  {
    type: 'cloudflare',
    name: 'Cloudflare',
    description: 'Global CDN & DNS provider',
    icon: CloudflareIcon,
    keywords: ['cloudflare', 'cdn', 'dns', 'global', 'cloud'],
  },
  {
    type: 'route53',
    name: 'AWS Route 53',
    description: 'Amazon Web Services DNS',
    icon: AwsRoute53Icon,
    keywords: ['aws', 'amazon', 'route53', 'route 53', 'amazon web services'],
  },
  {
    type: 'gcp',
    name: 'Google Cloud DNS',
    description: 'Google Cloud Platform DNS',
    icon: GoogleCloudIcon,
    keywords: ['gcp', 'google', 'google cloud', 'gcloud'],
  },
  {
    type: 'azure',
    name: 'Azure DNS',
    description: 'Microsoft Azure DNS',
    icon: AzureIcon,
    keywords: ['azure', 'microsoft', 'microsoft azure'],
  },
  {
    type: 'digitalocean',
    name: 'DigitalOcean',
    description: 'DigitalOcean DNS',
    icon: DigitalOceanIcon,
    keywords: ['digitalocean', 'digital ocean', 'do'],
  },
  {
    type: 'namecheap',
    name: 'Namecheap',
    description: 'Domain registrar & DNS',
    icon: NamecheapIcon,
    keywords: ['namecheap', 'domain', 'registrar'],
  },
]

// Wizard steps
type WizardStep = 'provider' | 'info' | 'credentials'

const STEPS: { id: WizardStep; label: string }[] = [
  { id: 'provider', label: 'Select Provider' },
  { id: 'info', label: 'Basic Information' },
  { id: 'credentials', label: 'Credentials' },
]

// Provider card component for selection
function ProviderCard({
  provider,
  selected,
  onClick,
}: {
  provider: ProviderInfo
  selected: boolean
  onClick: () => void
}) {
  const Icon = provider.icon
  return (
    <div
      className={cn(
        'cursor-pointer rounded-lg border p-4 transition-all hover:border-primary/50 hover:bg-accent/50',
        selected
          ? 'border-primary bg-primary/10 ring-1 ring-primary'
          : 'border-border bg-card'
      )}
      onClick={onClick}
    >
      <div className="flex items-center gap-3">
        <div
          className={cn(
            'flex h-10 w-10 items-center justify-center rounded-lg',
            selected
              ? 'bg-primary text-primary-foreground'
              : 'bg-muted text-muted-foreground'
          )}
        >
          <Icon className="h-5 w-5" />
        </div>
        <div>
          <h3 className="font-medium">{provider.name}</h3>
          <p className="text-sm text-muted-foreground">
            {provider.description}
          </p>
        </div>
      </div>
    </div>
  )
}

// Step indicator component
function StepIndicator({
  steps,
  currentStep,
}: {
  steps: { id: WizardStep; label: string }[]
  currentStep: WizardStep
}) {
  const currentIndex = steps.findIndex((s) => s.id === currentStep)

  return (
    <div className="flex items-center justify-center mb-8">
      {steps.map((step, index) => {
        const isCompleted = index < currentIndex
        const isCurrent = step.id === currentStep

        return (
          <div key={step.id} className="flex items-center">
            <div className="flex flex-col items-center">
              <div
                className={cn(
                  'flex h-8 w-8 items-center justify-center rounded-full border-2 text-sm font-medium transition-colors',
                  isCompleted
                    ? 'border-primary bg-primary text-primary-foreground'
                    : isCurrent
                      ? 'border-primary text-primary'
                      : 'border-muted text-muted-foreground'
                )}
              >
                {isCompleted ? (
                  <Check className="h-4 w-4" />
                ) : (
                  <span>{index + 1}</span>
                )}
              </div>
              <span
                className={cn(
                  'mt-1 text-xs',
                  isCurrent ? 'text-foreground font-medium' : 'text-muted-foreground'
                )}
              >
                {step.label}
              </span>
            </div>
            {index < steps.length - 1 && (
              <div
                className={cn(
                  'mx-2 h-0.5 w-12 transition-colors',
                  index < currentIndex ? 'bg-primary' : 'bg-muted'
                )}
              />
            )}
          </div>
        )
      })}
    </div>
  )
}

export function AddDnsProvider() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  // Wizard state
  const [currentStep, setCurrentStep] = useState<WizardStep>('provider')
  const [providerType, setProviderType] = useState<DnsProviderType | null>(null)
  const [searchQuery, setSearchQuery] = useState('')
  const [error, setError] = useState<string | null>(null)

  // Filter providers based on search query
  const filteredProviders = useMemo(() => {
    if (!searchQuery.trim()) return PROVIDERS

    const query = searchQuery.toLowerCase()
    return PROVIDERS.filter(
      (provider) =>
        provider.name.toLowerCase().includes(query) ||
        provider.description.toLowerCase().includes(query) ||
        provider.keywords.some((keyword) => keyword.includes(query))
    )
  }, [searchQuery])

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

  const route53Form = useForm<Route53FormData>({
    resolver: zodResolver(route53FormSchema),
    defaultValues: {
      name: '',
      description: '',
      access_key_id: '',
      secret_access_key: '',
      session_token: '',
      region: 'us-east-1',
    },
  })

  const digitaloceanForm = useForm<DigitalOceanFormData>({
    resolver: zodResolver(digitaloceanFormSchema),
    defaultValues: {
      name: '',
      description: '',
      api_token: '',
    },
  })

  const gcpForm = useForm<GcpFormData>({
    resolver: zodResolver(gcpFormSchema),
    defaultValues: {
      name: '',
      description: '',
      service_account_email: '',
      private_key: '',
      project_id: '',
    },
  })

  const azureForm = useForm<AzureFormData>({
    resolver: zodResolver(azureFormSchema),
    defaultValues: {
      name: '',
      description: '',
      tenant_id: '',
      client_id: '',
      client_secret: '',
      subscription_id: '',
      resource_group: '',
    },
  })

  const createProviderMut = useMutation({
    mutationFn: async (request: CreateDnsProviderRequest) => {
      const response = await createProvider({ body: request })
      return response.data
    },
    onSuccess: (provider) => {
      toast.success('DNS provider created successfully')
      queryClient.invalidateQueries({ queryKey: ['dnsProviders'] })
      if (provider) {
        navigate(`/dns-providers/${provider.id}`)
      }
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

  const handleProviderSelect = (type: DnsProviderType) => {
    setProviderType(type)
    setError(null)
  }

  const handleNext = async () => {
    if (currentStep === 'provider') {
      if (!providerType) {
        toast.error('Please select a provider')
        return
      }
      setCurrentStep('info')
    } else if (currentStep === 'info') {
      // Validate name field based on provider type
      let nameValid = false
      switch (providerType) {
        case 'cloudflare':
          nameValid = await cloudflareForm.trigger('name')
          break
        case 'namecheap':
          nameValid = await namecheapForm.trigger('name')
          break
        case 'route53':
          nameValid = await route53Form.trigger('name')
          break
        case 'digitalocean':
          nameValid = await digitaloceanForm.trigger('name')
          break
        case 'gcp':
          nameValid = await gcpForm.trigger('name')
          break
        case 'azure':
          nameValid = await azureForm.trigger('name')
          break
      }
      if (nameValid) {
        setCurrentStep('credentials')
      }
    }
  }

  const handleBack = () => {
    if (currentStep === 'info') {
      setCurrentStep('provider')
    } else if (currentStep === 'credentials') {
      setCurrentStep('info')
    }
  }

  const onCloudflareSubmit = (data: CloudflareFormData) => {
    setError(null)
    const credentials: ExtendedDnsProviderCredentials = {
      type: 'cloudflare',
      api_token: data.api_token,
      account_id: data.account_id || null,
    }
    const request = {
      name: data.name,
      provider_type: 'cloudflare',
      description: data.description || null,
      credentials,
    } as CreateDnsProviderRequest
    createProviderMut.mutate(request)
  }

  const onNamecheapSubmit = (data: NamecheapFormData) => {
    setError(null)
    const credentials: ExtendedDnsProviderCredentials = {
      type: 'namecheap',
      api_user: data.api_user,
      api_key: data.api_key,
      client_ip: data.client_ip || null,
      sandbox: data.sandbox,
    }
    const request = {
      name: data.name,
      provider_type: 'namecheap',
      description: data.description || null,
      credentials,
    } as CreateDnsProviderRequest
    createProviderMut.mutate(request)
  }

  const onRoute53Submit = (data: Route53FormData) => {
    setError(null)
    const credentials: ExtendedDnsProviderCredentials = {
      type: 'route53',
      access_key_id: data.access_key_id,
      secret_access_key: data.secret_access_key,
      session_token: data.session_token || null,
      region: data.region || null,
    }
    const request = {
      name: data.name,
      provider_type: 'route53',
      description: data.description || null,
      credentials,
    } as unknown as CreateDnsProviderRequest
    createProviderMut.mutate(request)
  }

  const onDigitalOceanSubmit = (data: DigitalOceanFormData) => {
    setError(null)
    const credentials: ExtendedDnsProviderCredentials = {
      type: 'digitalocean',
      api_token: data.api_token,
    }
    const request = {
      name: data.name,
      provider_type: 'digitalocean',
      description: data.description || null,
      credentials,
    } as unknown as CreateDnsProviderRequest
    createProviderMut.mutate(request)
  }

  const onGcpSubmit = (data: GcpFormData) => {
    setError(null)
    const credentials: ExtendedDnsProviderCredentials = {
      type: 'gcp',
      service_account_email: data.service_account_email,
      private_key: data.private_key,
      project_id: data.project_id,
    }
    const request = {
      name: data.name,
      provider_type: 'gcp',
      description: data.description || null,
      credentials,
    } as unknown as CreateDnsProviderRequest
    createProviderMut.mutate(request)
  }

  const onAzureSubmit = (data: AzureFormData) => {
    setError(null)
    const credentials: ExtendedDnsProviderCredentials = {
      type: 'azure',
      tenant_id: data.tenant_id,
      client_id: data.client_id,
      client_secret: data.client_secret,
      subscription_id: data.subscription_id,
      resource_group: data.resource_group,
    }
    const request = {
      name: data.name,
      provider_type: 'azure',
      description: data.description || null,
      credentials,
    } as unknown as CreateDnsProviderRequest
    createProviderMut.mutate(request)
  }

  const handleSubmit = () => {
    switch (providerType) {
      case 'cloudflare':
        cloudflareForm.handleSubmit(onCloudflareSubmit)()
        break
      case 'namecheap':
        namecheapForm.handleSubmit(onNamecheapSubmit)()
        break
      case 'route53':
        route53Form.handleSubmit(onRoute53Submit)()
        break
      case 'digitalocean':
        digitaloceanForm.handleSubmit(onDigitalOceanSubmit)()
        break
      case 'gcp':
        gcpForm.handleSubmit(onGcpSubmit)()
        break
      case 'azure':
        azureForm.handleSubmit(onAzureSubmit)()
        break
    }
  }

  const selectedProvider = PROVIDERS.find((p) => p.type === providerType)

  const handleEnterSubmit = useEnterSubmit(() => {
    if (currentStep === 'credentials') {
      handleSubmit()
    } else {
      handleNext()
    }
  })

  // Render basic information form fields based on provider type
  const renderBasicInfoFields = () => {
    if (!selectedProvider) return null

    switch (providerType) {
      case 'cloudflare':
        return (
          <Form {...cloudflareForm}>
            <div className="space-y-4">
              <FormField
                control={cloudflareForm.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input
                        placeholder={`My ${selectedProvider.name} Account`}
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
                        placeholder="DNS provider for production domains"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
          </Form>
        )

      case 'route53':
        return (
          <Form {...route53Form}>
            <div className="space-y-4">
              <FormField
                control={route53Form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input
                        placeholder={`My ${selectedProvider.name} Account`}
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
                control={route53Form.control}
                name="description"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Description (optional)</FormLabel>
                    <FormControl>
                      <Textarea
                        placeholder="DNS provider for production domains"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
          </Form>
        )

      case 'gcp':
        return (
          <Form {...gcpForm}>
            <div className="space-y-4">
              <FormField
                control={gcpForm.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input
                        placeholder={`My ${selectedProvider.name} Account`}
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
                control={gcpForm.control}
                name="description"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Description (optional)</FormLabel>
                    <FormControl>
                      <Textarea
                        placeholder="DNS provider for production domains"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
          </Form>
        )

      case 'azure':
        return (
          <Form {...azureForm}>
            <div className="space-y-4">
              <FormField
                control={azureForm.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input
                        placeholder={`My ${selectedProvider.name} Account`}
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
                control={azureForm.control}
                name="description"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Description (optional)</FormLabel>
                    <FormControl>
                      <Textarea
                        placeholder="DNS provider for production domains"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
          </Form>
        )

      case 'digitalocean':
        return (
          <Form {...digitaloceanForm}>
            <div className="space-y-4">
              <FormField
                control={digitaloceanForm.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input
                        placeholder={`My ${selectedProvider.name} Account`}
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
                control={digitaloceanForm.control}
                name="description"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Description (optional)</FormLabel>
                    <FormControl>
                      <Textarea
                        placeholder="DNS provider for production domains"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
          </Form>
        )

      case 'namecheap':
        return (
          <Form {...namecheapForm}>
            <div className="space-y-4">
              <FormField
                control={namecheapForm.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input
                        placeholder={`My ${selectedProvider.name} Account`}
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
                        placeholder="DNS provider for production domains"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
          </Form>
        )

      default:
        return null
    }
  }

  // Render credentials form fields based on provider type
  const renderCredentialsFields = () => {
    switch (providerType) {
      case 'cloudflare':
        return (
          <Form {...cloudflareForm}>
            <div className="space-y-4">
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
                      Create a token with Zone:Read and DNS:Edit permissions at
                      dash.cloudflare.com/profile/api-tokens
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
            </div>
          </Form>
        )

      case 'route53':
        return (
          <Form {...route53Form}>
            <div className="space-y-4">
              <FormField
                control={route53Form.control}
                name="access_key_id"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Access Key ID</FormLabel>
                    <FormControl>
                      <Input placeholder="AKIAIOSFODNN7EXAMPLE" {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={route53Form.control}
                name="secret_access_key"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Secret Access Key</FormLabel>
                    <FormControl>
                      <Input
                        type="password"
                        placeholder="Enter your AWS Secret Access Key"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={route53Form.control}
                name="session_token"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Session Token (optional)</FormLabel>
                    <FormControl>
                      <Input
                        type="password"
                        placeholder="For temporary credentials only"
                        {...field}
                      />
                    </FormControl>
                    <FormDescription>
                      Only required for temporary credentials (STS)
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={route53Form.control}
                name="region"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Region (optional)</FormLabel>
                    <FormControl>
                      <Input placeholder="us-east-1" {...field} />
                    </FormControl>
                    <FormDescription>
                      AWS region (defaults to us-east-1)
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
          </Form>
        )

      case 'gcp':
        return (
          <Form {...gcpForm}>
            <div className="space-y-4">
              <FormField
                control={gcpForm.control}
                name="project_id"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Project ID</FormLabel>
                    <FormControl>
                      <Input placeholder="my-gcp-project" {...field} />
                    </FormControl>
                    <FormDescription>Your GCP project ID</FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={gcpForm.control}
                name="service_account_email"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Service Account Email</FormLabel>
                    <FormControl>
                      <Input
                        type="email"
                        placeholder="dns-admin@my-project.iam.gserviceaccount.com"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={gcpForm.control}
                name="private_key"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Private Key</FormLabel>
                    <FormControl>
                      <Textarea
                        placeholder="-----BEGIN PRIVATE KEY-----&#10;...&#10;-----END PRIVATE KEY-----"
                        className="font-mono text-xs"
                        rows={6}
                        {...field}
                      />
                    </FormControl>
                    <FormDescription>
                      The private key from your service account JSON file
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
          </Form>
        )

      case 'azure':
        return (
          <Form {...azureForm}>
            <div className="space-y-4">
              <FormField
                control={azureForm.control}
                name="tenant_id"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Tenant ID</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
                        {...field}
                      />
                    </FormControl>
                    <FormDescription>
                      Azure Active Directory tenant ID
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={azureForm.control}
                name="client_id"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Client ID (Application ID)</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={azureForm.control}
                name="client_secret"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Client Secret</FormLabel>
                    <FormControl>
                      <Input
                        type="password"
                        placeholder="Enter your client secret"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={azureForm.control}
                name="subscription_id"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Subscription ID</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={azureForm.control}
                name="resource_group"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Resource Group</FormLabel>
                    <FormControl>
                      <Input placeholder="my-dns-resource-group" {...field} />
                    </FormControl>
                    <FormDescription>
                      Resource group containing your DNS zones
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
          </Form>
        )

      case 'digitalocean':
        return (
          <Form {...digitaloceanForm}>
            <div className="space-y-4">
              <FormField
                control={digitaloceanForm.control}
                name="api_token"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>API Token</FormLabel>
                    <FormControl>
                      <Input
                        type="password"
                        placeholder="Enter your DigitalOcean API token"
                        {...field}
                      />
                    </FormControl>
                    <FormDescription>
                      Create a token with read and write scope at
                      cloud.digitalocean.com/account/api/tokens
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </div>
          </Form>
        )

      case 'namecheap':
        return (
          <Form {...namecheapForm}>
            <div className="space-y-4">
              <FormField
                control={namecheapForm.control}
                name="api_user"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>API User</FormLabel>
                    <FormControl>
                      <Input placeholder="Your Namecheap username" {...field} />
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
                      <Input placeholder="Your whitelisted IP address" {...field} />
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
                      <FormLabel className="text-base">Sandbox Mode</FormLabel>
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
            </div>
          </Form>
        )

      default:
        return null
    }
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6 p-6 max-w-3xl mx-auto" onKeyDown={handleEnterSubmit}>
        {/* Header */}
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

        {/* Step Indicator */}
        <StepIndicator steps={STEPS} currentStep={currentStep} />

        {/* Step Content */}
        {currentStep === 'provider' && (
          <Card>
            <CardHeader>
              <CardTitle>Select Provider</CardTitle>
              <CardDescription>
                Choose your DNS provider to get started
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {/* Search box */}
              <div className="relative">
                <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  placeholder="Search providers..."
                  value={searchQuery}
                  onChange={(e) => setSearchQuery(e.target.value)}
                  className="pl-9"
                />
              </div>

              {/* Provider grid */}
              <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                {filteredProviders.map((provider) => (
                  <ProviderCard
                    key={provider.type}
                    provider={provider}
                    selected={providerType === provider.type}
                    onClick={() => handleProviderSelect(provider.type)}
                  />
                ))}
              </div>

              {filteredProviders.length === 0 && (
                <div className="text-center py-8 text-muted-foreground">
                  No providers found matching &ldquo;{searchQuery}&rdquo;
                </div>
              )}
            </CardContent>
          </Card>
        )}

        {currentStep === 'info' && selectedProvider && (
          <Card>
            <CardHeader>
              <div className="flex items-center gap-3">
                <div className="flex h-10 w-10 items-center justify-center rounded-lg bg-primary text-primary-foreground">
                  <selectedProvider.icon className="h-5 w-5" />
                </div>
                <div>
                  <CardTitle>Basic Information</CardTitle>
                  <CardDescription>
                    Give your {selectedProvider.name} provider a name to identify it
                  </CardDescription>
                </div>
              </div>
            </CardHeader>
            <CardContent>{renderBasicInfoFields()}</CardContent>
          </Card>
        )}

        {currentStep === 'credentials' && selectedProvider && (
          <Card>
            <CardHeader>
              <div className="flex items-center gap-3">
                <div className="flex h-10 w-10 items-center justify-center rounded-lg bg-primary text-primary-foreground">
                  <selectedProvider.icon className="h-5 w-5" />
                </div>
                <div>
                  <CardTitle>Credentials</CardTitle>
                  <CardDescription>
                    Enter your {selectedProvider.name} credentials
                  </CardDescription>
                </div>
              </div>
            </CardHeader>
            <CardContent>{renderCredentialsFields()}</CardContent>
          </Card>
        )}

        {/* Error Alert */}
        {error && (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        {/* Navigation Buttons */}
        <div className="flex justify-between">
          <Button
            type="button"
            variant="outline"
            onClick={currentStep === 'provider' ? () => navigate('/dns-providers') : handleBack}
          >
            {currentStep === 'provider' ? 'Cancel' : (
              <>
                <ArrowLeft className="mr-2 h-4 w-4" />
                Back
              </>
            )}
          </Button>

          {currentStep === 'credentials' ? (
            <Button
              type="button"
              onClick={handleSubmit}
              disabled={createProviderMut.isPending}
            >
              {createProviderMut.isPending && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              Create Provider
            </Button>
          ) : (
            <Button
              type="button"
              onClick={handleNext}
              disabled={currentStep === 'provider' && !providerType}
            >
              Next
              <ArrowRight className="ml-2 h-4 w-4" />
            </Button>
          )}
        </div>
      </div>
    </div>
  )
}
