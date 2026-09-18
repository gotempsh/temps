// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createNotificationEmailProviderMutation,
  createNotificationProviderMutation,
  createSlackProviderMutation,
  createWebhookProviderMutation,
} from '@/api/client/@tanstack/react-query.gen'
import {
  Button,
  Callout,
  PageContainer,
  Status,
  Wizard,
  useUrlState,
} from '@temps-sdk/ds'
import { Badge } from '@/components/ui/badge'

import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import {
  ArrowLeft,
  ArrowRight,
  Cloud,
  Mail,
  MoreHorizontal,
  Webhook,
} from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useForm } from 'react-hook-form'
import { FaSlack } from 'react-icons/fa'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import { ProviderForm } from '@/components/monitoring/ProviderForm'
import {
  ProviderFormData,
  providerSchema,
} from '@/components/monitoring/schemas'
import { cn } from '@/lib/utils'

type Step = 'provider-type' | 'configuration' | 'complete'
type ProviderType = 'email' | 'slack' | 'webhook' | 'cloudflare' | 'coming-soon'

interface ProviderOption {
  id: ProviderType
  name: string
  description: string
  icon: React.ReactNode
  available: boolean
}

const providerLabels: Partial<Record<ProviderType, string>> = {
  email: 'Email',
  slack: 'Slack',
  webhook: 'Webhook',
  cloudflare: 'Cloudflare Email',
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
    icon: <FaSlack className="h-6 w-6" />,
    available: true,
  },
  {
    id: 'webhook',
    name: 'Webhook',
    description:
      'Send JSON payloads to any HTTP endpoint for custom integrations',
    icon: <Webhook className="h-6 w-6" />,
    available: true,
  },
  {
    id: 'cloudflare',
    name: 'Cloudflare Email',
    description:
      'Send notification emails through Cloudflare Email Sending (no SMTP required)',
    icon: <Cloud className="h-6 w-6" />,
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
  const { get, patch } = useUrlState<'step' | 'provider'>()
  const selectedProvider =
    providerOptions.find(
      (option) => option.available && option.id === get('provider')
    )?.id ?? null
  const [complete, setComplete] = useState(false)
  const currentStep: Step = complete
    ? 'complete'
    : selectedProvider && get('step') === 'configuration'
      ? 'configuration'
      : 'provider-type'
  const setCurrentStep = (step: Step) => {
    if (step === 'complete') setComplete(true)
    else patch({ step })
  }

  const headingRef = useRef<HTMLHeadingElement>(null)
  useEffect(() => {
    headingRef.current?.focus()
  }, [currentStep])

  usePageTitle('Add Notification Provider')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Notification Providers', href: '/settings/notifications' },
      { label: 'Add Provider' },
    ])
  }, [setBreadcrumbs])

  const form = useForm<ProviderFormData>({
    resolver: zodResolver(providerSchema),
    defaultValues: {
      name: '',
      provider_type: (selectedProvider ??
        'email') as ProviderFormData['provider_type'],
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
        // Webhook config
        url: '',
        method: 'POST',
        headers: {},
        timeout_secs: 30,
      },
    },
  })

  useEffect(() => {
    if (selectedProvider && selectedProvider !== 'coming-soon') {
      form.setValue('provider_type', selectedProvider)
    }
  }, [selectedProvider, form])

  const createEmailMutation = useMutation({
    ...createNotificationEmailProviderMutation(),
    meta: {
      errorTitle: 'Failed to add email provider',
    },
    onSuccess: () => {
      setCurrentStep('complete')
      toast.success('Email provider added with a route for all notifications.')
    },
  })

  const createSlackMutation = useMutation({
    ...createSlackProviderMutation(),
    meta: {
      errorTitle: 'Failed to add Slack provider',
    },
    onSuccess: () => {
      setCurrentStep('complete')
      toast.success('Slack provider added with a route for all notifications.')
    },
  })

  const createWebhookMutation = useMutation({
    ...createWebhookProviderMutation(),
    meta: {
      errorTitle: 'Failed to add Webhook provider',
    },
    onSuccess: () => {
      setCurrentStep('complete')
      toast.success(
        'Webhook provider added with a route for all notifications.'
      )
    },
  })

  // Cloudflare uses the generic notification-provider endpoint (provider_type
  // + opaque config) rather than a dedicated typed mutation.
  const createCloudflareMutation = useMutation({
    ...createNotificationProviderMutation(),
    meta: {
      errorTitle: 'Failed to add Cloudflare provider',
    },
    onSuccess: () => {
      setCurrentStep('complete')
      toast.success(
        'Cloudflare provider added with a route for all notifications.'
      )
    },
  })

  useEffect(() => {
    if (currentStep !== 'complete') return
    const timer = setTimeout(
      () => navigate('/settings/notifications?tab=routes'),
      2000
    )
    return () => clearTimeout(timer)
  }, [currentStep, navigate])

  const handleProviderSelect = (provider: ProviderType) => {
    if (provider === 'coming-soon') return
    form.setValue(
      'provider_type',
      provider as 'email' | 'slack' | 'webhook' | 'cloudflare'
    )
    createEmailMutation.reset()
    createSlackMutation.reset()
    createWebhookMutation.reset()
    createCloudflareMutation.reset()
    patch({ provider, step: 'configuration' })
  }

  const handleBack = () => {
    if (currentStep === 'configuration') {
      setCurrentStep('provider-type')
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
    } else if (data.provider_type === 'webhook') {
      await createWebhookMutation.mutateAsync({
        body: {
          name: data.name,
          config: {
            url: data.config.url!,
            method: data.config.method || 'POST',
            headers: (data.config.headers || {}) as Record<string, string>,
            timeout_secs: data.config.timeout_secs || 30,
          },
        },
      })
    } else if (data.provider_type === 'cloudflare') {
      await createCloudflareMutation.mutateAsync({
        body: {
          name: data.name,
          provider_type: 'cloudflare',
          enabled: true,
          config: {
            account_id: data.config.account_id!,
            api_token: data.config.api_token!,
            from_address: data.config.from_address!,
            from_name: data.config.from_name || undefined,
            to_addresses: data.config.to_addresses!,
          },
        },
      })
    } else {
      await createEmailMutation.mutateAsync({
        body: {
          name: data.name,
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
    createEmailMutation.isPending ||
    createSlackMutation.isPending ||
    createWebhookMutation.isPending ||
    createCloudflareMutation.isPending

  const mutationError =
    createEmailMutation.error ??
    createSlackMutation.error ??
    createWebhookMutation.error ??
    createCloudflareMutation.error

  return (
    <PageContainer>
      <Wizard
        title="Add notification provider"
        description="Choose a delivery method and configure where Temps sends notifications."
        currentStep={currentStep}
        steps={[
          { id: 'provider-type', label: 'Choose provider' },
          { id: 'configuration', label: 'Configure' },
          { id: 'complete', label: 'Ready' },
        ]}
        footer={
          currentStep === 'configuration' ? (
            <>
              <Button variant="ghost" disabled={isLoading} onClick={handleBack}>
                <ArrowLeft className="size-4" /> Back
              </Button>
              <Button
                type="submit"
                form="add-notification-provider-form"
                busy={isLoading}
                busyLabel="Adding provider…"
              >
                Add provider
              </Button>
            </>
          ) : (
            <Button
              variant="outline"
              onClick={() =>
                navigate(
                  currentStep === 'complete'
                    ? '/settings/notifications?tab=routes'
                    : '/settings/notifications'
                )
              }
            >
              {currentStep === 'complete'
                ? 'View notification routes'
                : 'Cancel'}
            </Button>
          )
        }
      >
        {currentStep === 'provider-type' && (
          <div className="space-y-5">
            <h2
              ref={headingRef}
              tabIndex={-1}
              className="text-lg font-semibold outline-none"
            >
              How should notifications reach you?
            </h2>
            <div className="grid gap-3 sm:grid-cols-2">
              {providerOptions.map((provider) => (
                <Button
                  key={provider.id}
                  variant="outline"
                  disabled={!provider.available}
                  onClick={() => handleProviderSelect(provider.id)}
                  className={cn(
                    'h-auto justify-start gap-3 whitespace-normal p-4 text-left',
                    selectedProvider === provider.id && 'border-primary'
                  )}
                >
                  <span
                    aria-hidden="true"
                    className="flex size-10 shrink-0 items-center justify-center rounded-md border"
                  >
                    {provider.icon}
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="block font-medium">{provider.name}</span>
                    <span className="mt-1 block text-sm font-normal text-muted-foreground">
                      {provider.description}
                    </span>
                    {!provider.available && (
                      <Badge variant="secondary" className="mt-2">
                        Coming soon
                      </Badge>
                    )}
                  </span>
                  {provider.available && (
                    <ArrowRight
                      aria-hidden="true"
                      className="size-4 shrink-0"
                    />
                  )}
                </Button>
              ))}
            </div>
          </div>
        )}
        {currentStep === 'configuration' && selectedProvider && (
          <div className="space-y-6">
            <h2
              ref={headingRef}
              tabIndex={-1}
              className="text-lg font-semibold outline-none"
            >
              Configure {providerLabels[selectedProvider]}
            </h2>
            {mutationError && (
              <Callout tone="error" title="Could not add provider">
                Check the configuration and try again.{' '}
                {mutationError instanceof Error ? mutationError.message : ''}
              </Callout>
            )}
            <ProviderForm
              form={form}
              onSubmit={async (data) => {
                if (isLoading) return
                try {
                  await onSubmit(data)
                } catch {
                  /* Mutation state renders the retryable error above. */
                }
              }}
              isLoading={isLoading}
              isEdit={false}
              formId="add-notification-provider-form"
              hideSubmit
              hideProviderType
            />
          </div>
        )}
        {currentStep === 'complete' && (
          <div className="space-y-4">
            <h2
              ref={headingRef}
              tabIndex={-1}
              className="text-lg font-semibold outline-none"
            >
              Provider added
            </h2>
            <Status tone="ok" label="Ready to send notifications" />
            <p className="text-sm text-muted-foreground">
              A route for all notifications was created. Opening notification
              routes…
            </p>
          </div>
        )}
      </Wizard>
    </PageContainer>
  )
}
