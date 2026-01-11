import {
  createEmailProviderMutation,
  createSlackProviderMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import {
  ArrowLeft,
  ArrowRight,
  Bell,
  Check,
  Mail,
  MoreHorizontal,
  Slack,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { ProviderForm } from '@/components/monitoring/ProviderForm'
import {
  ProviderFormData,
  providerSchema,
} from '@/components/monitoring/schemas'
import { cn } from '@/lib/utils'

type Step = 'provider-type' | 'configuration' | 'complete'
type ProviderType = 'email' | 'slack' | 'coming-soon'

interface ProviderOption {
  id: ProviderType
  name: string
  description: string
  icon: React.ReactNode
  available: boolean
}

const providerOptions: ProviderOption[] = [
  {
    id: 'email',
    name: 'Email',
    description: 'Send notifications via SMTP email server',
    icon: <Mail className="h-6 w-6" />,
    available: true,
  },
  {
    id: 'slack',
    name: 'Slack',
    description: 'Send notifications to Slack channels via webhooks',
    icon: <Slack className="h-6 w-6" />,
    available: true,
  },
  {
    id: 'coming-soon',
    name: 'More Coming Soon',
    description: 'Additional providers like Discord, Teams, and more',
    icon: <MoreHorizontal className="h-6 w-6" />,
    available: false,
  },
]

export function AddNotificationProvider() {
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [currentStep, setCurrentStep] = useState<Step>('provider-type')
  const [selectedProvider, setSelectedProvider] = useState<ProviderType | null>(
    null
  )

  usePageTitle('Add Notification Provider')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Monitoring & Alerts', href: '/monitoring' },
      { label: 'Providers', href: '/notifications' },
      { label: 'Add Provider' },
    ])
  }, [setBreadcrumbs])

  const form = useForm<ProviderFormData>({
    resolver: zodResolver(providerSchema),
    defaultValues: {
      name: '',
      provider_type: 'email',
      config: {
        // Slack config
        webhook_url: '',
        channel: '',
        // Email config
        smtp_host: '',
        smtp_port: 587,
        use_credentials: false,
        smtp_username: '',
        password: '',
        from_name: '',
        from_address: '',
        to_addresses: [], // Array of email addresses
        tls_mode: 'Starttls', // Default TLS mode
        starttls_required: false,
        accept_invalid_certs: false,
      },
    },
  })

  const createEmailMutation = useMutation({
    ...createEmailProviderMutation(),
    meta: {
      errorTitle: 'Failed to add email provider',
    },
    onSuccess: () => {
      setCurrentStep('complete')
      toast.success('Email provider added successfully')
      setTimeout(() => {
        navigate('/notifications')
      }, 2000)
    },
  })

  const createSlackMutation = useMutation({
    ...createSlackProviderMutation(),
    meta: {
      errorTitle: 'Failed to add Slack provider',
    },
    onSuccess: () => {
      setCurrentStep('complete')
      toast.success('Slack provider added successfully')
      setTimeout(() => {
        navigate('/notifications')
      }, 2000)
    },
  })

  const handleProviderSelect = (provider: ProviderType) => {
    if (provider === 'coming-soon') return
    setSelectedProvider(provider)
    form.setValue('provider_type', provider as 'email' | 'slack')
    setCurrentStep('configuration')
  }

  const handleBack = () => {
    if (currentStep === 'configuration') {
      setCurrentStep('provider-type')
      setSelectedProvider(null)
    }
  }

  const onSubmit = async (data: ProviderFormData) => {
    if (data.provider_type === 'slack') {
      await createSlackMutation.mutateAsync({
        body: {
          name: data.name,
          enabled: true,
          config: {
            webhook_url: data.config.webhook_url!,
            channel: data.config.channel ?? null,
          },
        },
      })
    } else {
      await createEmailMutation.mutateAsync({
        body: {
          name: data.name,
          enabled: true,
          config: {
            smtp_host: data.config.smtp_host!,
            smtp_port: data.config.smtp_port!,
            username: data.config.use_credentials
              ? data.config.smtp_username || ''
              : '',
            password: data.config.use_credentials
              ? data.config.password || ''
              : '',
            from_address: data.config.from_address!,
            to_addresses: data.config.to_addresses!,
            from_name: data.config.from_name || undefined,
            tls_mode: data.config.tls_mode || undefined,
            starttls_required: data.config.starttls_required,
            accept_invalid_certs: data.config.accept_invalid_certs,
          },
        },
      })
    }
  }

  const isLoading =
    createEmailMutation.isPending || createSlackMutation.isPending

  const renderStepIndicator = () => {
    const steps = [
      { key: 'provider-type', label: 'Select Provider' },
      { key: 'configuration', label: 'Configure' },
      { key: 'complete', label: 'Complete' },
    ]

    const currentIndex = steps.findIndex((s) => s.key === currentStep)

    return (
      <div className="flex items-center justify-center mb-8">
        {steps.map((step, index) => (
          <div key={step.key} className="flex items-center">
            <div
              className={cn(
                'flex h-10 w-10 items-center justify-center rounded-full border-2 transition-colors',
                index <= currentIndex
                  ? 'border-primary bg-primary text-primary-foreground'
                  : 'border-muted-foreground/25 bg-background text-muted-foreground'
              )}
            >
              {index < currentIndex ? (
                <Check className="h-5 w-5" />
              ) : (
                <span className="text-sm font-medium">{index + 1}</span>
              )}
            </div>
            {index < steps.length - 1 && (
              <div
                className={cn(
                  'mx-2 h-0.5 w-16 transition-colors',
                  index < currentIndex ? 'bg-primary' : 'bg-muted-foreground/25'
                )}
              />
            )}
          </div>
        ))}
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="container max-w-5xl mx-auto py-6">
        {renderStepIndicator()}

        {currentStep === 'provider-type' && (
          <Card>
            <CardHeader>
              <CardTitle>Select Notification Provider</CardTitle>
              <CardDescription>
                Choose how you want to receive notifications about your
                deployments and infrastructure
              </CardDescription>
            </CardHeader>
            <CardContent>
              <div className="grid gap-4 md:grid-cols-3">
                {providerOptions.map((provider) => (
                  <Card
                    key={provider.id}
                    className={cn(
                      'cursor-pointer transition-all hover:shadow-md',
                      provider.available
                        ? 'hover:border-primary'
                        : 'opacity-50 cursor-not-allowed'
                    )}
                    onClick={() =>
                      provider.available && handleProviderSelect(provider.id)
                    }
                  >
                    <CardHeader className="pb-4">
                      <div className="flex items-center justify-between">
                        <div className="flex h-12 w-12 items-center justify-center rounded-lg bg-muted">
                          {provider.icon}
                        </div>
                        {provider.available && (
                          <ArrowRight className="h-5 w-5 text-muted-foreground" />
                        )}
                      </div>
                    </CardHeader>
                    <CardContent>
                      <h3 className="font-semibold mb-1">{provider.name}</h3>
                      <p className="text-sm text-muted-foreground">
                        {provider.description}
                      </p>
                    </CardContent>
                  </Card>
                ))}
              </div>
              <div className="mt-6 flex justify-between">
                <Button
                  variant="outline"
                  onClick={() => navigate('/notifications')}
                >
                  <ArrowLeft className="h-4 w-4 mr-2" />
                  Cancel
                </Button>
              </div>
            </CardContent>
          </Card>
        )}

        {currentStep === 'configuration' && selectedProvider && (
          <Card>
            <CardHeader>
              <CardTitle>
                Configure {selectedProvider === 'email' ? 'Email' : 'Slack'}{' '}
                Provider
              </CardTitle>
              <CardDescription>
                Enter the configuration details for your{' '}
                {selectedProvider === 'email' ? 'email' : 'Slack'} notification
                provider
              </CardDescription>
            </CardHeader>
            <CardContent>
              <ProviderForm
                form={form}
                onSubmit={onSubmit}
                isLoading={isLoading}
                isEdit={true}
              />
              <div className="mt-6 flex justify-between border-t pt-4">
                <Button
                  variant="outline"
                  onClick={handleBack}
                  disabled={isLoading}
                >
                  <ArrowLeft className="h-4 w-4 mr-2" />
                  Back
                </Button>
              </div>
            </CardContent>
          </Card>
        )}

        {currentStep === 'complete' && (
          <Card>
            <CardHeader>
              <div className="flex flex-col items-center justify-center py-8">
                <div className="flex h-20 w-20 items-center justify-center rounded-full bg-green-100 dark:bg-green-900/20 mb-4">
                  <Check className="h-10 w-10 text-green-600 dark:text-green-400" />
                </div>
                <CardTitle className="text-center">
                  Provider Added Successfully!
                </CardTitle>
                <CardDescription className="text-center mt-2">
                  Your notification provider has been configured and is ready to
                  send alerts
                </CardDescription>
              </div>
            </CardHeader>
            <CardContent className="flex justify-center">
              <Button onClick={() => navigate('/notifications')}>
                <Bell className="h-4 w-4 mr-2" />
                View Providers
              </Button>
            </CardContent>
          </Card>
        )}
      </div>
    </div>
  )
}
