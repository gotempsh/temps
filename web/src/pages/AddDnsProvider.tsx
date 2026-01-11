import {
  createProvider,
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
  Cloud,
  Globe,
  Loader2,
  Search,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'

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
  icon: React.ElementType
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

// AWS icon component
function AwsIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M6.763 10.036c0 .296.032.535.088.71.064.176.144.368.256.576.04.063.056.127.056.183 0 .08-.048.16-.152.24l-.503.335a.383.383 0 0 1-.208.072c-.08 0-.16-.04-.239-.112a2.47 2.47 0 0 1-.287-.375 6.18 6.18 0 0 1-.248-.471c-.622.734-1.405 1.101-2.347 1.101-.67 0-1.205-.191-1.596-.574-.391-.384-.59-.894-.59-1.533 0-.678.239-1.23.726-1.644.487-.415 1.133-.623 1.955-.623.272 0 .551.024.846.064.296.04.6.104.918.176v-.583c0-.607-.127-1.03-.375-1.277-.255-.248-.686-.367-1.3-.367-.28 0-.568.031-.863.103-.295.072-.583.16-.863.272a2.287 2.287 0 0 1-.28.104.488.488 0 0 1-.127.023c-.112 0-.168-.08-.168-.247v-.391c0-.128.016-.224.056-.28a.597.597 0 0 1 .224-.167c.279-.144.614-.264 1.005-.36a4.84 4.84 0 0 1 1.246-.151c.95 0 1.644.216 2.091.647.439.43.662 1.085.662 1.963v2.586zm-3.24 1.214c.263 0 .534-.048.822-.144.287-.096.543-.271.758-.51.128-.152.224-.32.272-.512.047-.191.08-.423.08-.694v-.335a6.66 6.66 0 0 0-.735-.136 6.02 6.02 0 0 0-.75-.048c-.535 0-.926.104-1.19.32-.263.215-.39.518-.39.917 0 .375.095.655.295.846.191.2.47.296.838.296zm6.41.862c-.144 0-.24-.024-.304-.08-.064-.048-.12-.16-.168-.311L7.586 5.55a1.398 1.398 0 0 1-.072-.32c0-.128.064-.2.191-.2h.783c.151 0 .255.025.31.08.065.048.113.16.16.312l1.342 5.284 1.245-5.284c.04-.16.088-.264.151-.312a.549.549 0 0 1 .32-.08h.638c.152 0 .256.025.32.08.063.048.12.16.151.312l1.261 5.348 1.381-5.348c.048-.16.104-.264.16-.312a.52.52 0 0 1 .311-.08h.743c.127 0 .2.065.2.2 0 .04-.009.08-.017.128a1.137 1.137 0 0 1-.056.2l-1.923 6.17c-.048.16-.104.263-.168.311a.51.51 0 0 1-.303.08h-.687c-.151 0-.255-.024-.32-.08-.063-.056-.119-.16-.15-.32l-1.238-5.148-1.23 5.14c-.04.16-.087.264-.15.32-.065.056-.177.08-.32.08zm10.256.215c-.415 0-.83-.048-1.229-.143-.399-.096-.71-.2-.918-.32-.128-.071-.215-.151-.247-.223a.563.563 0 0 1-.048-.224v-.407c0-.167.064-.247.183-.247.048 0 .096.008.144.024.048.016.12.048.2.08.271.12.566.215.878.279.319.064.63.096.95.096.502 0 .894-.088 1.165-.264a.86.86 0 0 0 .415-.758.777.777 0 0 0-.215-.559c-.144-.151-.415-.287-.806-.407l-1.157-.36c-.583-.183-1.014-.454-1.277-.813a1.902 1.902 0 0 1-.4-1.158c0-.335.073-.63.216-.886.144-.255.335-.479.575-.654.24-.184.51-.32.83-.415.32-.096.655-.136 1.006-.136.176 0 .359.008.535.032.183.024.35.056.518.088.16.04.312.08.455.127.144.048.256.096.336.144a.69.69 0 0 1 .24.2.43.43 0 0 1 .071.263v.375c0 .168-.064.256-.184.256a.83.83 0 0 1-.303-.096 3.652 3.652 0 0 0-1.532-.311c-.455 0-.815.071-1.062.223-.248.152-.375.383-.375.71 0 .224.08.416.24.567.159.152.454.304.877.44l1.134.358c.574.184.99.44 1.237.767.247.327.367.702.367 1.117 0 .343-.072.655-.207.926-.144.272-.336.511-.583.703-.248.2-.543.343-.886.447-.36.111-.734.167-1.142.167zM21.698 16.207c-2.626 1.94-6.442 2.969-9.722 2.969-4.598 0-8.74-1.7-11.87-4.526-.247-.223-.024-.527.27-.351 3.384 1.963 7.559 3.153 11.877 3.153 2.914 0 6.114-.607 9.06-1.852.439-.2.814.287.385.607zM22.792 14.961c-.336-.43-2.22-.207-3.074-.103-.255.032-.295-.192-.063-.36 1.5-1.053 3.967-.75 4.254-.399.287.36-.08 2.826-1.485 4.007-.216.184-.423.088-.327-.151.32-.79 1.03-2.57.695-2.994z" />
    </svg>
  )
}

// DigitalOcean icon component
function DigitalOceanIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M12.04 24v-4.78a7.22 7.22 0 0 0 0-14.44A7.23 7.23 0 0 0 4.71 12h4.82V7.17a4.89 4.89 0 1 1 2.51 9.09v4.78h.02-4.82v-3.64h-3.63v3.64H0v-3.64 3.64H0V12a12 12 0 1 1 12.04 12z" />
    </svg>
  )
}

// Google Cloud icon component
function GcpIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M12.19 2.38a9.344 9.344 0 0 0-9.234 6.893c.053-.02-.055.013 0 0-3.875 2.551-3.922 8.11-.247 10.941l.006-.007-.007.03a6.717 6.717 0 0 0 4.077 1.356h5.173l.03.03h5.192c6.687.053 9.376-8.605 3.835-12.35a9.365 9.365 0 0 0-8.825-6.893zM8.073 19.28a4.407 4.407 0 0 1-2.463-4.014c.013-.03.03-.042.03-.064v-.043c0-.263.264-1.26.264-1.26l.03-.03.01-.03a4.392 4.392 0 0 1 2.403-2.633l.026-.012v.02a2.643 2.643 0 0 1 .95-.187c.69 0 1.33.266 1.807.698a5.44 5.44 0 0 1 1.108 1.61 4.413 4.413 0 0 1-4.165 5.944zm8.12-2.065a2.643 2.643 0 0 1-.95.187c-.702 0-1.358-.276-1.83-.732l.004-.007a5.308 5.308 0 0 1-1.1-1.586 4.413 4.413 0 0 1 4.166-5.944 4.38 4.38 0 0 1 2.462 4.015v.042c0 .264-.264 1.26-.264 1.26l-.03.03-.01.03a4.404 4.404 0 0 1-2.448 2.704z" />
    </svg>
  )
}

// Azure icon component
function AzureIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
    >
      <path d="M5.483 21.3H24L14.025 4.013l-3.038 8.347 5.836 6.938L5.483 21.3zM13.23 2.7L6.105 8.677 0 19.253h5.505v.014L13.23 2.7z" />
    </svg>
  )
}

// Provider data
const PROVIDERS: ProviderInfo[] = [
  {
    type: 'cloudflare',
    name: 'Cloudflare',
    description: 'Global CDN & DNS provider',
    icon: Cloud,
    keywords: ['cloudflare', 'cdn', 'dns', 'global', 'cloud'],
  },
  {
    type: 'route53',
    name: 'AWS Route 53',
    description: 'Amazon Web Services DNS',
    icon: AwsIcon,
    keywords: ['aws', 'amazon', 'route53', 'route 53', 'amazon web services'],
  },
  {
    type: 'gcp',
    name: 'Google Cloud DNS',
    description: 'Google Cloud Platform DNS',
    icon: GcpIcon,
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
    icon: Globe,
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
      <div className="space-y-6 p-6 max-w-3xl mx-auto">
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
