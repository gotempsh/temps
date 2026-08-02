import {
  createEmailProvider as createProviderApi,
  type CreateEmailProviderRequest,
  type EmailProviderResponse,
} from '@/api/client'
import { AWSIcon } from '@/components/icons/AWSIcon'
import { ScalewayIcon } from '@/components/icons/ScalewayIcon'
import {
  awsRegions,
  problemMessage,
  providerTypeLabel,
  scalewayRegions,
} from '@/components/email/sharedUtils'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
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
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, Check, Loader2, Server } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

type ProviderType = 'ses' | 'scaleway' | 'smtp'
type Step = 'type' | 'configure'

const createProviderSchema = z
  .object({
    name: z.string().min(1, 'Name is required'),
    provider_type: z.enum(['ses', 'scaleway', 'smtp']),
    region: z.string().min(1, 'Region is required'),
    // SES credentials
    sns_topic_arn: z.string().optional(),
    access_key_id: z.string().optional(),
    secret_access_key: z.string().optional(),
    // Scaleway credentials
    api_key: z.string().optional(),
    project_id: z.string().optional(),
    // SMTP credentials
    smtp_host: z.string().optional(),
    smtp_port: z.number().int().min(1).max(65535).optional(),
    smtp_username: z.string().optional(),
    smtp_password: z.string().optional(),
    smtp_encryption: z.enum(['starttls', 'tls', 'none']).optional(),
    smtp_accept_invalid_certs: z.boolean().optional(),
  })
  .superRefine((data, ctx) => {
    if (data.provider_type === 'ses') {
      if (!data.access_key_id || !data.secret_access_key) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message:
            'Access key ID and secret access key are required for AWS SES',
          path: ['access_key_id'],
        })
      }
    } else if (data.provider_type === 'scaleway') {
      if (!data.api_key || !data.project_id) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: 'API key and project ID are required for Scaleway',
          path: ['api_key'],
        })
      }
    } else if (data.provider_type === 'smtp') {
      if (!data.smtp_host) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: 'SMTP host is required',
          path: ['smtp_host'],
        })
      }
      if (!data.smtp_port) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: 'SMTP port is required',
          path: ['smtp_port'],
        })
      }
      if (data.smtp_username && !data.smtp_password) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: 'Password is required when a username is provided',
          path: ['smtp_password'],
        })
      }
    }
  })

type CreateProviderFormData = z.infer<typeof createProviderSchema>

async function createEmailProvider(
  data: CreateProviderFormData
): Promise<EmailProviderResponse> {
  const body: CreateEmailProviderRequest = {
    name: data.name,
    provider_type: data.provider_type,
    region: data.region,
  }

  if (
    data.provider_type === 'ses' &&
    data.access_key_id &&
    data.secret_access_key
  ) {
    body.sns_topic_arn = data.sns_topic_arn || undefined
    body.ses_credentials = {
      access_key_id: data.access_key_id,
      secret_access_key: data.secret_access_key,
    }
  } else if (
    data.provider_type === 'scaleway' &&
    data.api_key &&
    data.project_id
  ) {
    body.scaleway_credentials = {
      api_key: data.api_key,
      project_id: data.project_id,
    }
  } else if (
    data.provider_type === 'smtp' &&
    data.smtp_host &&
    data.smtp_port
  ) {
    body.smtp_credentials = {
      host: data.smtp_host,
      port: data.smtp_port,
      username: data.smtp_username || undefined,
      password: data.smtp_password || undefined,
      encryption: data.smtp_encryption ?? 'starttls',
      accept_invalid_certs: data.smtp_accept_invalid_certs ?? false,
    }
  }

  const response = await createProviderApi({ body })
  if (response.error || !response.data) {
    throw new Error(
      problemMessage(response.error, 'Failed to create email provider')
    )
  }
  return response.data
}

interface ProviderOption {
  id: ProviderType
  name: string
  tagline: string
  description: string
  icon: React.ReactNode
  requirements: string[]
  defaultRegion: string
}

const providerOptions: ProviderOption[] = [
  {
    id: 'ses',
    name: 'AWS SES',
    tagline: 'Fully managed by Temps',
    description:
      'Send through Amazon Simple Email Service. Temps manages domain verification, DKIM/SPF DNS records, and tracks bounces and complaints.',
    icon: <AWSIcon className="size-6 text-[#FF9900]" />,
    requirements: [
      'AWS access key with SES permissions',
      'AWS secret access key',
      'Optional SNS topic for delivery events',
    ],
    defaultRegion: 'us-east-1',
  },
  {
    id: 'scaleway',
    name: 'Scaleway',
    tagline: 'European transactional email',
    description:
      'Send through Scaleway Transactional Email. EU-hosted infrastructure with domain verification and DNS managed by Temps.',
    icon: <ScalewayIcon className="size-6 text-[#4F0599]" />,
    requirements: [
      'Scaleway secret key with email permissions',
      'Scaleway project ID',
    ],
    defaultRegion: 'fr-par',
  },
  {
    id: 'smtp',
    name: 'SMTP',
    tagline: 'Import an existing provider',
    description:
      'Use SMTP credentials you already have (AWS SES SMTP, Sendgrid, Mailgun, …). Your sending domain must already be verified at the upstream provider — Temps will not manage DNS.',
    icon: <Server className="size-6 text-slate-600 dark:text-slate-300" />,
    requirements: [
      'SMTP host and port',
      'SMTP username and password',
      'Domain verified at the upstream provider',
    ],
    defaultRegion: 'custom',
  },
]

// ============================================================================
// Step 1 — provider type selection
// ============================================================================

function ProviderTypeCards({
  onSelect,
}: {
  onSelect: (option: ProviderOption) => void
}) {
  return (
    <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
      {providerOptions.map((option) => (
        <button
          key={option.id}
          type="button"
          onClick={() => onSelect(option)}
          className="group flex flex-col rounded-xl border border-border bg-card text-left hover:border-primary/40 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
        >
          <div className="flex items-center gap-3 p-5 pb-0">
            <div className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-muted">
              {option.icon}
            </div>
            <div className="min-w-0">
              <h3 className="font-semibold">{option.name}</h3>
              <p className="text-xs text-muted-foreground">{option.tagline}</p>
            </div>
          </div>
          <p className="p-5 pb-4 text-sm text-muted-foreground text-pretty">
            {option.description}
          </p>
          <div className="mt-auto border-t border-border/60 p-5 pt-4">
            <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
              You&apos;ll need
            </p>
            <ul role="list" className="mt-2 space-y-1.5">
              {option.requirements.map((req) => (
                <li
                  key={req}
                  className="flex items-start gap-2 text-sm text-muted-foreground"
                >
                  <span className="flex h-lh items-center">
                    <Check className="size-4 shrink-0 text-primary" />
                  </span>
                  {req}
                </li>
              ))}
            </ul>
          </div>
        </button>
      ))}
    </div>
  )
}

// ============================================================================
// Page
// ============================================================================

export function AddEmailProvider() {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [step, setStep] = useState<Step>('type')
  const headingRef = useRef<HTMLHeadingElement>(null)
  const [selected, setSelected] = useState<ProviderOption | null>(null)

  usePageTitle('Add Email Provider')

  // Selecting a type (or going back) unmounts the focused button; move focus
  // to the new step's heading so keyboard and screen-reader users aren't
  // dropped to <body>.
  useEffect(() => {
    headingRef.current?.focus()
  }, [step])

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Email', href: '/email' },
      { label: 'Providers', href: '/email?tab=providers' },
      { label: 'Add Provider' },
    ])
  }, [setBreadcrumbs])

  const form = useForm<CreateProviderFormData>({
    resolver: zodResolver(createProviderSchema),
    defaultValues: {
      name: '',
      provider_type: 'ses',
      region: 'us-east-1',
      sns_topic_arn: '',
      access_key_id: '',
      secret_access_key: '',
      api_key: '',
      project_id: '',
      smtp_host: '',
      smtp_port: 587,
      smtp_username: '',
      smtp_password: '',
      smtp_encryption: 'starttls',
      smtp_accept_invalid_certs: false,
    },
  })

  const createMutation = useMutation({
    mutationFn: createEmailProvider,
    onSuccess: (provider) => {
      toast.success('Email provider created', {
        description: `${provider.name} is ready to send transactional emails.`,
      })
      queryClient.invalidateQueries({ queryKey: ['email-providers'] })
      navigate('/email?tab=providers')
    },
    onError: (error: Error) => {
      toast.error('Failed to create provider', {
        description: error.message,
      })
    },
  })

  const handleSelect = (option: ProviderOption) => {
    setSelected(option)
    form.setValue('provider_type', option.id)
    form.setValue('region', option.defaultRegion)
    setStep('configure')
  }

  const handleBack = () => {
    setStep('type')
    setSelected(null)
  }

  const onSubmit = (data: CreateProviderFormData) => {
    createMutation.mutate(data)
  }

  const providerType = selected?.id
  const regions =
    providerType === 'ses'
      ? awsRegions
      : providerType === 'scaleway'
        ? scalewayRegions
        : []

  return (
    <div className="flex-1 overflow-auto">
      <div className="px-4 py-6 sm:px-6">
        {step === 'type' && (
          <>
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="mb-4 -ml-2 text-muted-foreground"
              onClick={() => navigate('/email?tab=providers')}
            >
              <ArrowLeft className="mr-2 size-4" />
              Back to providers
            </Button>

            <div className="mb-6">
              <p className="text-sm font-medium text-muted-foreground">
                Step 1 of 2
              </p>
              <h1
                ref={headingRef}
                tabIndex={-1}
                className="mt-1 text-2xl font-semibold tracking-tight text-balance outline-none"
              >
                Choose a provider type
              </h1>
              <p className="mt-1 text-muted-foreground text-pretty">
                How should Temps send transactional emails? You&apos;ll
                configure the credentials in the next step.
              </p>
            </div>

            <ProviderTypeCards onSelect={handleSelect} />
          </>
        )}

        {step === 'configure' && selected && (
          <div className="mx-auto max-w-2xl">
            <div className="mb-6">
              <p className="text-sm font-medium text-muted-foreground">
                Step 2 of 2
              </p>
              <h1
                ref={headingRef}
                tabIndex={-1}
                className="mt-1 text-2xl font-semibold tracking-tight text-balance outline-none"
              >
                Configure {providerTypeLabel(selected.id)}
              </h1>
              <p className="mt-1 text-muted-foreground text-pretty">
                {selected.id === 'smtp'
                  ? 'Enter the SMTP credentials from your upstream email provider.'
                  : `Enter the credentials Temps should use to send through ${providerTypeLabel(selected.id)}.`}
              </p>
            </div>

            <Card>
              <CardHeader className="flex-row items-center justify-between space-y-0 border-b py-4">
                <div className="flex items-center gap-3">
                  <div className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted">
                    {selected.icon}
                  </div>
                  <div>
                    <p className="text-sm font-medium">{selected.name}</p>
                    <p className="text-xs text-muted-foreground">
                      {selected.tagline}
                    </p>
                  </div>
                </div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={handleBack}
                >
                  Change
                </Button>
              </CardHeader>
              <CardContent className="pt-6">
                <Form {...form}>
                  <form
                    onSubmit={form.handleSubmit(onSubmit)}
                    className="space-y-6"
                  >
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

                    {providerType !== 'smtp' && (
                      <FormField
                        control={form.control}
                        name="region"
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel>Region</FormLabel>
                            <Select
                              onValueChange={field.onChange}
                              value={field.value}
                            >
                              <FormControl>
                                <SelectTrigger>
                                  <SelectValue placeholder="Select a region" />
                                </SelectTrigger>
                              </FormControl>
                              <SelectContent>
                                {regions.map((region) => (
                                  <SelectItem
                                    key={region.value}
                                    value={region.value}
                                  >
                                    <div className="flex w-full items-center justify-between gap-4">
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
                    )}

                    {providerType === 'ses' && (
                      <>
                        <FormField
                          control={form.control}
                          name="access_key_id"
                          render={({ field }) => (
                            <FormItem>
                              <FormLabel>Access Key ID</FormLabel>
                              <FormControl>
                                <Input
                                  placeholder="AKIAIOSFODNN7EXAMPLE"
                                  autoComplete="off"
                                  {...field}
                                />
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
                                  autoComplete="new-password"
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

                        <FormField
                          control={form.control}
                          name="sns_topic_arn"
                          render={({ field }) => (
                            <FormItem>
                              <FormLabel>SNS Topic ARN (optional)</FormLabel>
                              <FormControl>
                                <Input
                                  placeholder="arn:aws:sns:us-east-1:123456789012:temps-events"
                                  autoComplete="off"
                                  {...field}
                                />
                              </FormControl>
                              <FormDescription>
                                Exact SNS topic authorized to send SES delivery,
                                bounce, and complaint events for this provider.
                                You can also set this up automatically later
                                from the provider&apos;s detail page.
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
                                  autoComplete="new-password"
                                  {...field}
                                />
                              </FormControl>
                              <FormDescription>
                                Your Scaleway secret key with Transactional
                                Email permissions.
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
                                  autoComplete="off"
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

                    {providerType === 'smtp' && (
                      <>
                        <div className="rounded-md border border-amber-200 bg-amber-50 p-3 text-sm text-amber-900 dark:border-amber-900/40 dark:bg-amber-950/30 dark:text-amber-100">
                          Domains added under an SMTP provider are treated as
                          already verified — Temps cannot manage DKIM/SPF/MX
                          records via SMTP. Make sure DNS is configured at your
                          upstream provider (e.g. the AWS SES console) before
                          sending.
                        </div>

                        <div className="grid grid-cols-1 gap-4 sm:grid-cols-[1fr_120px]">
                          <FormField
                            control={form.control}
                            name="smtp_host"
                            render={({ field }) => (
                              <FormItem>
                                <FormLabel>SMTP Host</FormLabel>
                                <FormControl>
                                  <Input
                                    placeholder="email-smtp.eu-west-1.amazonaws.com"
                                    autoComplete="off"
                                    {...field}
                                  />
                                </FormControl>
                                <FormDescription>
                                  For AWS SES:{' '}
                                  <code className="font-mono text-xs">
                                    email-smtp.&lt;region&gt;.amazonaws.com
                                  </code>
                                  .
                                </FormDescription>
                                <FormMessage />
                              </FormItem>
                            )}
                          />

                          <FormField
                            control={form.control}
                            name="smtp_port"
                            render={({ field }) => (
                              <FormItem>
                                <FormLabel>Port</FormLabel>
                                <FormControl>
                                  <Input
                                    type="number"
                                    min={1}
                                    max={65535}
                                    placeholder="587"
                                    value={field.value ?? ''}
                                    onChange={(e) => {
                                      const v = e.target.value
                                      field.onChange(
                                        v === '' ? undefined : Number(v)
                                      )
                                    }}
                                    onBlur={field.onBlur}
                                    name={field.name}
                                    ref={field.ref}
                                  />
                                </FormControl>
                                <FormMessage />
                              </FormItem>
                            )}
                          />
                        </div>

                        <FormField
                          control={form.control}
                          name="smtp_encryption"
                          render={({ field }) => (
                            <FormItem>
                              <FormLabel>Encryption</FormLabel>
                              <Select
                                onValueChange={(value) => {
                                  field.onChange(value)
                                  // Suggest the conventional port when switching
                                  // modes, unless the user has customised it.
                                  const port = form.getValues('smtp_port')
                                  if (
                                    value === 'starttls' &&
                                    (port === 465 || port === 25)
                                  ) {
                                    form.setValue('smtp_port', 587)
                                  } else if (
                                    value === 'tls' &&
                                    (port === 587 || port === 25)
                                  ) {
                                    form.setValue('smtp_port', 465)
                                  } else if (
                                    value === 'none' &&
                                    (port === 587 || port === 465)
                                  ) {
                                    form.setValue('smtp_port', 25)
                                  }
                                }}
                                value={field.value ?? 'starttls'}
                              >
                                <FormControl>
                                  <SelectTrigger>
                                    <SelectValue placeholder="Select TLS mode" />
                                  </SelectTrigger>
                                </FormControl>
                                <SelectContent>
                                  <SelectItem value="starttls">
                                    STARTTLS (port 587, default)
                                  </SelectItem>
                                  <SelectItem value="tls">
                                    Implicit TLS / SMTPS (port 465)
                                  </SelectItem>
                                  <SelectItem value="none">
                                    No encryption (local testing only)
                                  </SelectItem>
                                </SelectContent>
                              </Select>
                              <FormMessage />
                            </FormItem>
                          )}
                        />

                        <FormField
                          control={form.control}
                          name="smtp_username"
                          render={({ field }) => (
                            <FormItem>
                              <FormLabel>Username (optional)</FormLabel>
                              <FormControl>
                                <Input
                                  placeholder="AKIAIOSFODNN7EXAMPLE"
                                  autoComplete="off"
                                  {...field}
                                />
                              </FormControl>
                              <FormDescription>
                                For AWS SES SMTP, this is the SMTP user
                                generated in the SES console (it is <em>not</em>{' '}
                                your IAM access key).
                              </FormDescription>
                              <FormMessage />
                            </FormItem>
                          )}
                        />

                        <FormField
                          control={form.control}
                          name="smtp_password"
                          render={({ field }) => (
                            <FormItem>
                              <FormLabel>Password / SMTP secret</FormLabel>
                              <FormControl>
                                <Input
                                  type="password"
                                  placeholder="••••••••••••"
                                  autoComplete="new-password"
                                  {...field}
                                />
                              </FormControl>
                              <FormMessage />
                            </FormItem>
                          )}
                        />
                      </>
                    )}

                    <div className="flex items-center justify-between border-t pt-4">
                      <Button
                        type="button"
                        variant="outline"
                        onClick={handleBack}
                        disabled={createMutation.isPending}
                      >
                        <ArrowLeft className="mr-2 size-4" />
                        Back
                      </Button>
                      <Button type="submit" disabled={createMutation.isPending}>
                        {createMutation.isPending && (
                          <Loader2 className="mr-2 size-4 animate-spin" />
                        )}
                        Add Provider
                      </Button>
                    </div>
                  </form>
                </Form>
              </CardContent>
            </Card>
          </div>
        )}
      </div>
    </div>
  )
}
