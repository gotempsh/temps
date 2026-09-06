// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createEmailDomain as createEmailDomainSdk,
  importEmailDomain as importEmailDomainSdk,
  listEmailProviders as listEmailProvidersSdk,
  type CreateEmailDomainRequest,
  type EmailDomainWithDnsResponse,
  type EmailProviderResponse,
  type ImportEmailDomainRequest,
} from '@/api/client'
import { EmailProviderLogo, type EmailProviderType } from '@/components/ui/email-provider-logo'
import { problemMessage } from '@/components/email/sharedUtils'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { cn } from '@/lib/utils'
import {
  ArrowLeft,
  ArrowRight,
  Check,
  Download,
  Globe,
  Loader2,
  Plus,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

// ============================================================================
// Schemas (reused from EmailDomainsManagement)
// ============================================================================

const createDomainSchema = z.object({
  provider_id: z.number().min(1, 'Provider is required'),
  domain: z
    .string()
    .min(1, 'Domain is required')
    .regex(
      /^[a-zA-Z0-9][a-zA-Z0-9-_.]*\.[a-zA-Z]{2,}$/,
      'Please enter a valid domain (e.g., mail.example.com)'
    ),
})

const importDomainSchema = z.object({
  provider_id: z.number().min(1, 'Provider is required'),
  domain: z
    .string()
    .min(1, 'Domain is required')
    .regex(
      /^[a-zA-Z0-9][a-zA-Z0-9-_.]*\.[a-zA-Z]{2,}$/,
      'Please enter a valid domain (e.g., mail.example.com)'
    ),
  provider_identity_id: z.string().optional(),
})

type CreateDomainFormData = z.infer<typeof createDomainSchema>
type ImportDomainFormData = z.infer<typeof importDomainSchema>
type EmailProvider = EmailProviderResponse
type EmailDomainWithDns = EmailDomainWithDnsResponse

// ============================================================================
// API functions
// ============================================================================

async function createEmailDomain(
  data: CreateDomainFormData
): Promise<EmailDomainWithDns> {
  const body: CreateEmailDomainRequest = {
    provider_id: data.provider_id,
    domain: data.domain,
  }
  const response = await createEmailDomainSdk({ body })
  if (response.error || !response.data) {
    throw new Error(problemMessage(response.error, 'Failed to create email domain'))
  }
  return response.data
}

async function importEmailDomain(data: ImportDomainFormData): Promise<EmailDomainWithDns> {
  const body: ImportEmailDomainRequest = {
    provider_id: data.provider_id,
    domain: data.domain,
    provider_identity_id: data.provider_identity_id ?? null,
  }
  const response = await importEmailDomainSdk({ body })
  if (response.error || !response.data) {
    const msg = problemMessage(response.error, 'Failed to import email domain')
    const status =
      response.error &&
      typeof response.error === 'object' &&
      'status' in response.error
        ? (response.error as { status?: number }).status
        : undefined
    const err = new Error(msg) as Error & { httpStatus?: number }
    err.httpStatus = status
    throw err
  }
  return response.data
}

async function listEmailProviders(): Promise<EmailProvider[]> {
  const response = await listEmailProvidersSdk()
  if (response.error) {
    throw new Error(problemMessage(response.error, 'Failed to fetch email providers'))
  }
  return response.data ?? []
}

// ============================================================================
// Step indicator — visual pattern from SetupWizardShell
// ============================================================================

type WizardStep = 'choose-mode' | 'configure' | 'review'

const WIZARD_STEPS: Array<{ id: WizardStep; label: string }> = [
  { id: 'choose-mode', label: 'Choose mode' },
  { id: 'configure', label: 'Configure' },
  { id: 'review', label: 'Review' },
]

const STEP_ORDER: WizardStep[] = ['choose-mode', 'configure', 'review']

function WizardStepIndicator({ currentStep }: { currentStep: WizardStep }) {
  const currentIndex = STEP_ORDER.indexOf(currentStep)

  return (
    <ol
      role="list"
      className="flex items-center justify-center gap-2 sm:gap-4"
    >
      {WIZARD_STEPS.map((step, index) => {
        const stepIndex = STEP_ORDER.indexOf(step.id)
        const isDone = stepIndex < currentIndex
        const isActive = step.id === currentStep
        const isLast = index === WIZARD_STEPS.length - 1
        return (
          <li key={step.id} className="flex items-center gap-2 sm:gap-4">
            <div className="flex items-center gap-2">
              <span
                className={cn(
                  'flex size-7 shrink-0 items-center justify-center rounded-full border text-xs font-medium tabular-nums transition-colors',
                  isDone && 'border-emerald-500 bg-emerald-500 text-white',
                  isActive &&
                    !isDone &&
                    'border-primary bg-primary text-primary-foreground',
                  !isDone &&
                    !isActive &&
                    'border-muted-foreground/30 text-muted-foreground'
                )}
              >
                {isDone ? (
                  <Check className="size-4" strokeWidth={3} />
                ) : (
                  index + 1
                )}
              </span>
              <span
                className={cn(
                  'hidden text-sm font-medium sm:inline',
                  isDone || isActive
                    ? 'text-foreground'
                    : 'text-muted-foreground'
                )}
              >
                {step.label}
              </span>
            </div>
            {!isLast && (
              <span
                aria-hidden
                className={cn(
                  'h-px w-8 sm:w-12',
                  isDone ? 'bg-emerald-500' : 'bg-border'
                )}
              />
            )}
          </li>
        )
      })}
    </ol>
  )
}

// ============================================================================
// Step 1 — Choose mode cards
// ============================================================================

type DomainMode = 'create' | 'import'

interface ModeOption {
  id: DomainMode
  title: string
  description: string
  details: string
  icon: React.ReactNode
}

const MODE_OPTIONS: ModeOption[] = [
  {
    id: 'create',
    title: 'Create new domain',
    description: 'Provision a new sending domain with your provider',
    details:
      'Temps will register the domain identity with your provider and generate the DNS records you need to add. Best for domains you have not set up for email yet.',
    icon: <Plus className="size-6" />,
  },
  {
    id: 'import',
    title: 'Import existing domain',
    description: 'Adopt a domain already configured in your provider console',
    details:
      'Temps will fetch the current state of a domain identity you created directly in your provider. DNS records stay as-is; Temps tracks the status and lets you verify from here.',
    icon: <Download className="size-6" />,
  },
]

function ChooseModeStep({
  selected,
  onSelect,
}: {
  selected: DomainMode | null
  onSelect: (mode: DomainMode) => void
}) {
  return (
    <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
      {MODE_OPTIONS.map((option) => {
        const isSelected = selected === option.id
        return (
          <button
            key={option.id}
            type="button"
            onClick={() => onSelect(option.id)}
            className={cn(
              'group flex flex-col rounded-xl border bg-card text-left transition-all focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary',
              isSelected
                ? 'border-primary ring-2 ring-primary'
                : 'border-border hover:border-primary/40'
            )}
          >
            <div className="flex items-start gap-4 p-5">
              <div
                className={cn(
                  'flex size-11 shrink-0 items-center justify-center rounded-lg transition-colors',
                  isSelected
                    ? 'bg-primary text-primary-foreground'
                    : 'bg-muted text-muted-foreground group-hover:bg-primary/10 group-hover:text-primary'
                )}
              >
                {option.icon}
              </div>
              <div className="min-w-0">
                <h3 className="font-semibold">{option.title}</h3>
                <p className="mt-0.5 text-sm text-muted-foreground">
                  {option.description}
                </p>
              </div>
            </div>
            <p className="border-t px-5 py-4 text-sm text-muted-foreground text-pretty">
              {option.details}
            </p>
          </button>
        )
      })}
    </div>
  )
}

// ============================================================================
// Step 2 — Configure
// ============================================================================

interface Step2Errors {
  provider_id?: string
  domain?: string
  provider_identity_id?: string
}

function ConfigureStep({
  mode,
  providers,
  providerId,
  domain,
  providerIdentityId,
  errors,
  onProviderChange,
  onDomainChange,
  onProviderIdentityIdChange,
}: {
  mode: DomainMode
  providers: EmailProvider[]
  providerId: number | undefined
  domain: string
  providerIdentityId: string
  errors: Step2Errors
  onProviderChange: (id: number) => void
  onDomainChange: (value: string) => void
  onProviderIdentityIdChange: (value: string) => void
}) {
  const selectedProvider = providers.find((p) => p.id === providerId)
  const isScaleway = selectedProvider?.provider_type === 'scaleway'

  return (
    <Card>
      <CardContent className="pt-6 space-y-6">
        {/* Provider select */}
        <div className="space-y-2">
          <Label htmlFor="provider-select">Provider</Label>
          <Select
            value={providerId?.toString() ?? ''}
            onValueChange={(value) => onProviderChange(parseInt(value))}
          >
            <SelectTrigger id="provider-select" className={errors.provider_id ? 'border-destructive' : ''}>
              {selectedProvider ? (
                <div className="flex items-center gap-2">
                  <EmailProviderLogo
                    provider={selectedProvider.provider_type as EmailProviderType}
                    size={20}
                  />
                  <span>{selectedProvider.name}</span>
                </div>
              ) : (
                <SelectValue placeholder="Select a provider" />
              )}
            </SelectTrigger>
            <SelectContent>
              {providers.map((provider) => (
                <SelectItem key={provider.id} value={provider.id.toString()}>
                  <div className="flex items-center gap-2">
                    <EmailProviderLogo
                      provider={provider.provider_type as EmailProviderType}
                      size={20}
                    />
                    <span>{provider.name}</span>
                  </div>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {errors.provider_id && (
            <p className="text-sm text-destructive">{errors.provider_id}</p>
          )}
          <p className="text-sm text-muted-foreground">
            {mode === 'create'
              ? 'The email provider to use for this domain.'
              : 'The email provider where this domain is registered.'}
          </p>
        </div>

        {/* Domain input */}
        <div className="space-y-2">
          <Label htmlFor="domain-input">Domain</Label>
          <Input
            id="domain-input"
            placeholder="send.example.com"
            value={domain}
            onChange={(e) => onDomainChange(e.target.value)}
            className={errors.domain ? 'border-destructive' : ''}
            autoComplete="off"
          />
          {errors.domain && (
            <p className="text-sm text-destructive">{errors.domain}</p>
          )}
          <p className="text-sm text-muted-foreground">
            {mode === 'create'
              ? 'Use a subdomain (e.g., send.example.com) to isolate your email sending reputation and protect your primary domain.'
              : 'The domain name as it appears in your email provider.'}
          </p>
        </div>

        {/* Provider identity ID — import mode only */}
        {mode === 'import' && (
          <div className="space-y-2">
            <Label htmlFor="identity-id-input">
              Provider identity ID{' '}
              {isScaleway ? (
                <span className="font-normal text-muted-foreground">
                  (required for Scaleway)
                </span>
              ) : (
                <span className="font-normal text-muted-foreground">
                  (optional)
                </span>
              )}
            </Label>
            <Input
              id="identity-id-input"
              placeholder="12345678-1234-1234-1234-123456789012"
              value={providerIdentityId}
              onChange={(e) => onProviderIdentityIdChange(e.target.value)}
              className={errors.provider_identity_id ? 'border-destructive' : ''}
              autoComplete="off"
            />
            {errors.provider_identity_id && (
              <p className="text-sm text-destructive">
                {errors.provider_identity_id}
              </p>
            )}
            <p className="text-sm text-muted-foreground">
              {isScaleway
                ? 'The domain UUID shown in the Scaleway console (Transactional Email → Domains). Required so Temps can look up the correct identity.'
                : 'The provider-internal UUID for this domain identity. Required for Scaleway; not needed for SES (which uses the domain name for lookups).'}
            </p>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// ============================================================================
// Step 3 — Review
// ============================================================================

function ReviewStep({
  mode,
  providers,
  providerId,
  domain,
  providerIdentityId,
  isPending,
  onSubmit,
}: {
  mode: DomainMode
  providers: EmailProvider[]
  providerId: number | undefined
  domain: string
  providerIdentityId: string
  isPending: boolean
  onSubmit: () => void
}) {
  const provider = providers.find((p) => p.id === providerId)

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          {mode === 'create' ? 'Create new domain' : 'Import existing domain'}
        </CardTitle>
        <CardDescription>
          {mode === 'create'
            ? 'Review the details below, then click "Add domain" to register and generate DNS records.'
            : 'Review the details below, then click "Import domain" to fetch the current state from your provider.'}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-3 text-sm">
          <dt className="text-muted-foreground">Mode</dt>
          <dd className="font-medium capitalize">{mode === 'create' ? 'Create new' : 'Import existing'}</dd>

          <dt className="text-muted-foreground">Provider</dt>
          <dd>
            {provider ? (
              <div className="flex items-center gap-2">
                <EmailProviderLogo
                  provider={provider.provider_type as EmailProviderType}
                  size={18}
                />
                <span className="font-medium">{provider.name}</span>
                <span className="rounded border bg-muted/50 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-muted-foreground">
                  {provider.provider_type}
                </span>
              </div>
            ) : (
              <span className="text-muted-foreground">—</span>
            )}
          </dd>

          <dt className="text-muted-foreground">Domain</dt>
          <dd className="font-mono font-medium">{domain || '—'}</dd>

          {mode === 'import' && providerIdentityId && (
            <>
              <dt className="text-muted-foreground">Identity ID</dt>
              <dd className="font-mono text-xs break-all">{providerIdentityId}</dd>
            </>
          )}
        </dl>

        <div className="pt-2">
          <Button
            onClick={onSubmit}
            disabled={isPending}
            className="w-full sm:w-auto"
          >
            {isPending && <Loader2 className="mr-2 size-4 animate-spin" />}
            {mode === 'create' ? 'Add domain' : 'Import domain'}
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}

// ============================================================================
// Page
// ============================================================================

export function EmailDomainNew() {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()

  usePageTitle('Add Email Domain')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Email', href: '/email' },
      { label: 'Domains', href: '/email?tab=domains' },
      { label: 'Add Domain' },
    ])
  }, [setBreadcrumbs])

  // Wizard state
  const [currentStep, setCurrentStep] = useState<WizardStep>('choose-mode')
  const [mode, setMode] = useState<DomainMode | null>(null)

  // Step 2 field state — controlled inputs to avoid react-hook-form
  // complexity across step transitions in a wizard.
  const [providerId, setProviderId] = useState<number | undefined>(undefined)
  const [domain, setDomain] = useState('')
  const [providerIdentityId, setProviderIdentityId] = useState('')
  const [step2Errors, setStep2Errors] = useState<Step2Errors>({})

  // Providers query
  const { data: providers = [], isLoading: isLoadingProviders } = useQuery({
    queryKey: ['email-providers'],
    queryFn: listEmailProviders,
  })
  const isSelectedProviderScaleway =
    providers.find((p) => p.id === providerId)?.provider_type === 'scaleway'

  const createMutation = useMutation({
    mutationFn: createEmailDomain,
    onSuccess: (data) => {
      toast.success('Domain added', {
        description: 'Configure the DNS records to finish verification.',
      })
      queryClient.invalidateQueries({ queryKey: ['email-domains'] })
      queryClient.setQueryData(['email-domain', data.domain.id], data)
      navigate(`/email/domains/${data.domain.id}`)
    },
    onError: (error: Error) => {
      toast.error('Failed to add domain', { description: error.message })
    },
  })

  const importMutation = useMutation({
    mutationFn: importEmailDomain,
    onSuccess: (data) => {
      const isVerified = data.domain.status === 'verified'
      toast.success('Domain imported', {
        description: isVerified
          ? 'DNS is already verified — your domain is ready to use.'
          : 'Check the DNS records to complete verification.',
      })
      queryClient.invalidateQueries({ queryKey: ['email-domains'] })
      queryClient.setQueryData(['email-domain', data.domain.id], data)
      navigate(`/email/domains/${data.domain.id}`)
    },
    onError: (error: Error & { httpStatus?: number }) => {
      if (error.httpStatus === 409) {
        toast.error('Domain already registered', {
          description:
            'This domain is already tracked in Temps for this provider. Find it in the list or verify the provider ID.',
        })
      } else {
        toast.error('Failed to import domain', { description: error.message })
      }
    },
  })

  const isPending = createMutation.isPending || importMutation.isPending

  // Step 2 validation using the same Zod schemas
  const validateStep2 = (): boolean => {
    const schema =
      mode === 'create' ? createDomainSchema : importDomainSchema
    const result = schema.safeParse({
      provider_id: providerId,
      domain,
      provider_identity_id: providerIdentityId || undefined,
    })

    if (!result.success) {
      const errors: Step2Errors = {}
      result.error.issues.forEach((issue) => {
        const field = issue.path[0] as keyof Step2Errors
        if (!errors[field]) {
          errors[field] = issue.message
        }
      })
      setStep2Errors(errors)
      return false
    }

    // Zod can't see the selected provider's type, so it can't make
    // provider_identity_id conditionally required — Scaleway has no way to
    // look up an existing identity by domain name alone, so without a UUID
    // here the import would fail server-side with a provisioning-sounding
    // error that doesn't describe this missing input at all.
    if (mode === 'import' && isSelectedProviderScaleway && !providerIdentityId.trim()) {
      setStep2Errors({
        provider_identity_id: 'A Scaleway domain UUID is required to import this domain',
      })
      return false
    }

    setStep2Errors({})
    return true
  }

  const handleNext = () => {
    if (currentStep === 'choose-mode') {
      if (!mode) return
      setCurrentStep('configure')
    } else if (currentStep === 'configure') {
      if (validateStep2()) {
        setCurrentStep('review')
      }
    }
  }

  const handleBack = () => {
    if (currentStep === 'configure') {
      setCurrentStep('choose-mode')
    } else if (currentStep === 'review') {
      setCurrentStep('configure')
    }
  }

  const handleSubmit = () => {
    if (!mode || providerId === undefined) return

    if (mode === 'create') {
      createMutation.mutate({ provider_id: providerId, domain })
    } else {
      importMutation.mutate({
        provider_id: providerId,
        domain,
        provider_identity_id: providerIdentityId || undefined,
      })
    }
  }

  const canGoNext =
    currentStep === 'choose-mode'
      ? mode !== null
      : currentStep === 'configure'
        ? true // validated on click
        : false

  return (
    <div className="flex-1 overflow-auto">
      <div className="px-4 py-6 sm:px-6">
        {/* Back link */}
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="mb-6 -ml-2 text-muted-foreground"
          onClick={() => navigate('/email?tab=domains')}
        >
          <ArrowLeft className="mr-2 size-4" />
          Back to domains
        </Button>

        {/* Title + step indicator */}
        <div className="mx-auto max-w-3xl space-y-8">
          <div className="space-y-2 text-center">
            <h1 className="text-2xl font-semibold tracking-tight text-balance">
              Add email domain
            </h1>
            <p className="text-sm text-muted-foreground text-pretty">
              Register a sending domain with your email provider.
            </p>
          </div>

          <WizardStepIndicator currentStep={currentStep} />

          {/* Step content */}
          <div>
            {currentStep === 'choose-mode' && (
              <div className="space-y-4">
                <div>
                  <h2 className="text-base font-semibold">
                    How do you want to add this domain?
                  </h2>
                  <p className="mt-1 text-sm text-muted-foreground">
                    Choose whether you are registering a brand-new domain identity or
                    bringing in one you already created in your provider console.
                  </p>
                </div>
                <ChooseModeStep selected={mode} onSelect={setMode} />
              </div>
            )}

            {currentStep === 'configure' && mode && (
              <div className="space-y-4">
                <div>
                  <h2 className="text-base font-semibold">
                    {mode === 'create'
                      ? 'Configure your new domain'
                      : 'Configure the domain to import'}
                  </h2>
                  <p className="mt-1 text-sm text-muted-foreground">
                    {mode === 'create'
                      ? 'Choose your provider and enter the domain you want to set up for sending.'
                      : 'Enter the provider and exact domain name as it appears in your provider console.'}
                  </p>
                </div>
                {isLoadingProviders ? (
                  <Card>
                    <CardContent className="pt-6 space-y-6">
                      <div className="h-10 animate-pulse rounded-md bg-muted" />
                      <div className="h-10 animate-pulse rounded-md bg-muted" />
                    </CardContent>
                  </Card>
                ) : providers.length === 0 ? (
                  <Card>
                    <CardContent className="pt-6 flex flex-col items-center gap-3 py-10">
                      <Globe className="size-8 text-muted-foreground" />
                      <div className="text-center">
                        <p className="font-medium">No email providers configured</p>
                        <p className="mt-1 text-sm text-muted-foreground">
                          You need to add a provider before setting up a domain.{' '}
                          <button
                            type="button"
                            className="underline underline-offset-2 hover:text-foreground"
                            onClick={() => navigate('/email/providers/new')}
                          >
                            Add a provider
                          </button>
                        </p>
                      </div>
                    </CardContent>
                  </Card>
                ) : (
                  <ConfigureStep
                    mode={mode}
                    providers={providers}
                    providerId={providerId}
                    domain={domain}
                    providerIdentityId={providerIdentityId}
                    errors={step2Errors}
                    onProviderChange={(id) => {
                      setProviderId(id)
                      setStep2Errors((prev) => ({ ...prev, provider_id: undefined }))
                    }}
                    onDomainChange={(value) => {
                      setDomain(value)
                      setStep2Errors((prev) => ({ ...prev, domain: undefined }))
                    }}
                    onProviderIdentityIdChange={(value) => {
                      setProviderIdentityId(value)
                      setStep2Errors((prev) => ({
                        ...prev,
                        provider_identity_id: undefined,
                      }))
                    }}
                  />
                )}
              </div>
            )}

            {currentStep === 'review' && mode && (
              <ReviewStep
                mode={mode}
                providers={providers}
                providerId={providerId}
                domain={domain}
                providerIdentityId={providerIdentityId}
                isPending={isPending}
                onSubmit={handleSubmit}
              />
            )}
          </div>

          {/* Navigation buttons */}
          <div className="flex items-center justify-between gap-3">
            <div>
              {currentStep !== 'choose-mode' && (
                <Button
                  type="button"
                  variant="outline"
                  onClick={handleBack}
                  disabled={isPending}
                >
                  <ArrowLeft className="mr-2 size-4" />
                  Back
                </Button>
              )}
            </div>

            <div className="flex items-center gap-3">
              <Button
                type="button"
                variant="outline"
                onClick={() => navigate('/email?tab=domains')}
                disabled={isPending}
              >
                Cancel
              </Button>

              {currentStep !== 'review' && (
                <Button
                  type="button"
                  onClick={handleNext}
                  disabled={!canGoNext}
                >
                  Next
                  <ArrowRight className="ml-2 size-4" />
                </Button>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  )
}
