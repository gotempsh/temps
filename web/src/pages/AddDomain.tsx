// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getCertificateMethodsOptions,
  createCustomDomainMutation,
  createDomainMutation,
  getEnvironmentsOptions,
  getProjectsOptions,
  setIngressHostnameChoiceMutation,
  updateIngressConnectorMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { CloudflareTokenChecklist } from '@/components/domains/CloudflareTokenGuide'
import { ingressSetupUrl } from '@/components/nodes/ingress-setup'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
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
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { PageHeader } from '@/components/layout/PageContainer'
import { usePageTitle } from '@/hooks/usePageTitle'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  CheckCircle,
  Cloud,
  Globe,
  Info,
  Shield,
  ArrowLeft,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { Link, useNavigate } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import { useEnterSubmit } from '@/hooks/useEnterSubmit'
import { problemDetail } from '@/lib/api-problem'
import { certificateChoices } from '@/lib/reachability'
import {
  chosenTunnel,
  levelsBelow,
  tunnelChoice,
  tunnelsCovering,
  allTunnelsOptions,
} from '@/lib/tunnel-domains'

const domainWizardSchema = z.object({
  domain: z
    .string()
    .min(1, 'Domain is required')
    .regex(
      /^(\*\.)?[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?)*\.[a-zA-Z]{2,}$/,
      'Invalid domain format (e.g., example.com or *.example.com)'
    ),
  // `http-01`, `dns-01`, or `tunnel:<id>` (ADR 058 D10).
  challengeType: z
    .string()
    .regex(/^(http-01|dns-01|tunnel:\d+)$/, 'Choose how it gets HTTPS'),
})

type DomainWizardFormData = z.infer<typeof domainWizardSchema>

export function AddDomain() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const [step, setStep] = useState<'domain' | 'challenge' | 'confirm'>('domain')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Domains', href: '/domains' },
      { label: 'Add domain' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Add domain')

  const form = useForm<DomainWizardFormData>({
    resolver: zodResolver(domainWizardSchema),
    defaultValues: {
      domain: '',
      challengeType: 'http-01',
    },
  })

  const connectorsQuery = useQuery({
    ...allTunnelsOptions(),
    retry: false,
  })
  // HTTP-01 needs something answering on port 80 (ADR 064 D4).
  const certificateMethods = useQuery({
    ...getCertificateMethodsOptions(),
    retry: false,
    staleTime: 60_000,
  })
  const http01Available = certificateMethods.data?.http01_available
  const [attachProject, setAttachProject] = useState<string>('')
  const [attachEnvironment, setAttachEnvironment] = useState<string>('')
  const projectsQuery = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 100 } }),
    staleTime: 60_000,
  })
  const environmentsQuery = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: Number(attachProject) },
    }),
    enabled: !!attachProject,
  })
  const chooseTunnel = useMutation(setIngressHostnameChoiceMutation())
  const publishAll = useMutation(updateIngressConnectorMutation())
  const attach = useMutation(createCustomDomainMutation())

  const createDomain = useMutation({
    ...createDomainMutation(),
    meta: {
      errorTitle: 'Failed to create domain',
    },
    onSuccess: (data) => {
      toast.success(
        'Domain created successfully! Continue with SSL provisioning.'
      )
      // Navigate to domain detail page to complete the wizard
      navigate(`/domains/${data.id}`)
    },
  })

  const handleNext = () => {
    if (step === 'domain') {
      form.trigger('domain').then((valid) => {
        if (valid) {
          const domain = form.getValues('domain')
          const covering = tunnelsCovering(domain, connectorsQuery.data ?? [])
          if (covering.length > 0) {
            // A tunnel carries it: no certificate to issue.
            form.setValue('challengeType', tunnelChoice(covering[0].id))
          } else if (domain.startsWith('*.')) {
            // Auto-select DNS-01 for wildcard domains
            form.setValue('challengeType', 'dns-01')
          } else if (chosenTunnel(form.getValues('challengeType')) != null) {
            form.setValue('challengeType', 'http-01')
          }
          setStep('challenge')
        }
      })
    } else if (step === 'challenge') {
      form.trigger('challengeType').then((valid) => {
        if (valid) setStep('confirm')
      })
    }
  }

  const handleBack = () => {
    if (step === 'challenge') setStep('domain')
    else if (step === 'confirm') setStep('challenge')
  }

  const addThroughTunnel = async (
    data: DomainWizardFormData,
    connectorId: number
  ) => {
    const domain = data.domain.trim().toLowerCase()
    const connector = connectorsQuery.data?.find((c) => c.id === connectorId)
    try {
      if (domain.startsWith('*.')) {
        // Every name under it is what "publish everything" already means.
        if (connector && !connector.auto_publish) {
          await publishAll.mutateAsync({
            path: { id: connectorId },
            body: { auto_publish: true },
          })
        }
      } else {
        await chooseTunnel.mutateAsync({
          path: { hostname: domain },
          body: { mode: 'tunnel', connector_id: connectorId },
        })
        if (attachProject && attachEnvironment) {
          await attach.mutateAsync({
            path: { project_id: Number(attachProject) },
            body: { domain, environment_id: Number(attachEnvironment) },
          })
        }
      }
    } catch (error) {
      toast.error(`Could not add ${domain}`, {
        description: problemDetail(error, 'Try again in a moment.'),
      })
      return
    }
    toast.success(
      domain.startsWith('*.') || (attachProject && attachEnvironment)
        ? `${domain} goes through ${connector?.name ?? 'the tunnel'}. Its DNS record appears within a minute.`
        : `${domain} will go through ${connector?.name ?? 'the tunnel'} once you add it to an environment.`
    )
    navigate('/domains')
  }

  const handleSubmit = async (data: DomainWizardFormData) => {
    const connectorId = chosenTunnel(data.challengeType)
    if (connectorId != null) {
      await addThroughTunnel(data, connectorId)
      return
    }
    await createDomain.mutateAsync({
      body: {
        domain: data.domain,
        challenge_type: data.challengeType,
      },
    })
  }

  const watchedValues = useWatch({ control: form.control })
  const covering = tunnelsCovering(
    watchedValues.domain ?? '',
    connectorsQuery.data ?? []
  )
  const tunnel = connectorsQuery.data?.find(
    (c) => c.id === chosenTunnel(watchedValues.challengeType)
  )
  const wildcardInput = !!watchedValues.domain?.trim().startsWith('*.')
  const tooDeep = tunnel
    ? levelsBelow(watchedValues.domain ?? '', tunnel) > 1
    : false
  const submitting =
    createDomain.isPending ||
    chooseTunnel.isPending ||
    publishAll.isPending ||
    attach.isPending

  const handleEnterSubmit = useEnterSubmit(() => {
    if (step === 'confirm') {
      form.handleSubmit(handleSubmit)()
    } else {
      handleNext()
    }
  })

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6 p-4 sm:p-6" onKeyDown={handleEnterSubmit}>
        {/* Header */}
        <div className="flex items-start gap-4">
          <Button
            aria-label="Back"
            variant="ghost"
            size="icon"
            onClick={() => navigate('/domains')}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <PageHeader
            className="min-w-0 flex-1"
            title="Add domain"
            description="Add a custom domain and choose how it gets HTTPS: a certificate from Let's Encrypt, or a tunnel that brings its own"
          />
        </div>

        <div>
          <Form {...form}>
            <form
              onSubmit={form.handleSubmit(handleSubmit)}
              className="space-y-6"
              onKeyDown={(e) => {
                if (e.key === 'Enter' && step !== 'confirm') {
                  e.preventDefault()
                  handleNext()
                }
              }}
            >
              {/* Step 1: Domain */}
              {step === 'domain' && (
                <div className="space-y-4">
                  <div className="flex items-center gap-2 text-sm text-muted-foreground">
                    <div className="flex items-center gap-2">
                      <div className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs font-medium text-primary-foreground">
                        1
                      </div>
                      <span className="font-medium">Domain</span>
                    </div>
                    <div className="h-px flex-1 bg-border" />
                    <div className="flex items-center gap-2">
                      <div className="flex h-6 w-6 items-center justify-center rounded-full bg-muted text-xs font-medium text-muted-foreground">
                        2
                      </div>
                      <span>HTTPS</span>
                    </div>
                    <div className="h-px flex-1 bg-border" />
                    <div className="flex items-center gap-2">
                      <div className="flex h-6 w-6 items-center justify-center rounded-full bg-muted text-xs font-medium text-muted-foreground">
                        3
                      </div>
                      <span>Confirm</span>
                    </div>
                  </div>

                  <FormField
                    control={form.control}
                    name="domain"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Domain name</FormLabel>
                        <FormControl>
                          <Input
                            {...field}
                            placeholder="example.com or *.example.com"
                            autoFocus
                          />
                        </FormControl>
                        <FormDescription>
                          Enter your custom domain (e.g., example.com,
                          app.example.com, or *.example.com for wildcard)
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <Alert>
                    <Info className="h-4 w-4" />
                    <AlertDescription>
                      {covering.length > 0
                        ? `It's under ${covering[0].domain}, which tunnel ${covering[0].name} carries: it can go through the tunnel with no DNS records or certificate to set up.`
                        : "You'll need to point your domain's DNS records to your server after creation. We'll provide detailed instructions in the next steps."}
                    </AlertDescription>
                  </Alert>

                  <div className="flex justify-between gap-2">
                    <Button
                      type="button"
                      variant="outline"
                      onClick={() => navigate('/domains')}
                    >
                      Cancel
                    </Button>
                    <Button type="button" onClick={handleNext}>
                      Next
                    </Button>
                  </div>
                </div>
              )}

              {/* Step 2: Challenge Type */}
              {step === 'challenge' && (
                <div className="space-y-4">
                  <div className="flex items-center gap-2 text-sm text-muted-foreground">
                    <div className="flex items-center gap-2">
                      <div className="flex h-6 w-6 items-center justify-center rounded-full bg-muted text-xs font-medium text-muted-foreground">
                        <CheckCircle className="h-4 w-4 text-green-600" />
                      </div>
                      <span>Domain</span>
                    </div>
                    <div className="h-px flex-1 bg-border" />
                    <div className="flex items-center gap-2">
                      <div className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs font-medium text-primary-foreground">
                        2
                      </div>
                      <span className="font-medium">HTTPS</span>
                    </div>
                    <div className="h-px flex-1 bg-border" />
                    <div className="flex items-center gap-2">
                      <div className="flex h-6 w-6 items-center justify-center rounded-full bg-muted text-xs font-medium text-muted-foreground">
                        3
                      </div>
                      <span>Confirm</span>
                    </div>
                  </div>

                  <FormField
                    control={form.control}
                    name="challengeType"
                    render={({ field }) => {
                      const isWildcard = watchedValues.domain?.startsWith('*.')
                      const choices = certificateChoices({
                        coveringTunnels: covering.length,
                        wildcard: !!isWildcard,
                        http01Available,
                      })
                      return (
                        <FormItem className="space-y-4">
                          <FormLabel>How it gets HTTPS</FormLabel>
                          <FormDescription>
                            {covering.length > 0
                              ? "A tunnel covers this domain, so its provider can serve HTTPS with no certificate here. Or issue one from Let's Encrypt."
                              : isWildcard
                                ? 'Wildcard domains require DNS-01 challenge'
                                : "Choose how you want to verify domain ownership for Let's Encrypt"}
                          </FormDescription>
                          <FormControl>
                            <RadioGroup
                              onValueChange={field.onChange}
                              value={field.value}
                              className="grid gap-4"
                            >
                              {covering.map((connector, index) => {
                                const value = tunnelChoice(connector.id)
                                return (
                                  <Card
                                    key={connector.id}
                                    className={`relative cursor-pointer border-2 transition-colors ${field.value === value ? 'border-primary' : 'border-border hover:border-primary/50'}`}
                                  >
                                    <label className="flex cursor-pointer items-start gap-4 p-4">
                                      <RadioGroupItem
                                        value={value}
                                        id={value}
                                        className="mt-1"
                                      />
                                      <div className="flex-1 space-y-2">
                                        <div className="flex items-center gap-2">
                                          <Cloud className="h-5 w-5 text-primary" />
                                          <div className="font-semibold">
                                            Through tunnel {connector.name}
                                            {index === 0 && ' (Recommended)'}
                                          </div>
                                        </div>
                                        <p className="text-sm text-muted-foreground">
                                          The tunnel carries every name under{' '}
                                          {connector.domain}. Temps creates the
                                          DNS record and the provider serves
                                          HTTPS, so there is no certificate to
                                          issue and no port to open.
                                        </p>
                                        <div className="space-y-1 text-sm">
                                          <div className="flex items-center gap-2 text-green-700 dark:text-green-600">
                                            <CheckCircle className="h-4 w-4" />
                                            <span>
                                              DNS record created for you
                                            </span>
                                          </div>
                                          <div className="flex items-center gap-2 text-green-700 dark:text-green-600">
                                            <CheckCircle className="h-4 w-4" />
                                            <span>
                                              HTTPS from the tunnel&apos;s
                                              provider
                                            </span>
                                          </div>
                                          <div className="flex items-center gap-2 text-green-700 dark:text-green-600">
                                            <CheckCircle className="h-4 w-4" />
                                            <span>
                                              Works behind NAT, no open ports
                                            </span>
                                          </div>
                                        </div>
                                      </div>
                                    </label>
                                  </Card>
                                )
                              })}
                              {choices.showHttp01 && (
                                <Card
                                  className={`relative cursor-pointer border-2 transition-colors ${field.value === 'http-01' ? 'border-primary' : 'border-border hover:border-primary/50'}`}
                                >
                                  <label className="flex cursor-pointer items-start gap-4 p-4">
                                    <RadioGroupItem
                                      value="http-01"
                                      id="http-01"
                                      className="mt-1"
                                    />
                                    <div className="flex-1 space-y-2">
                                      <div className="flex items-center gap-2">
                                        <Globe className="h-5 w-5 text-primary" />
                                        <div className="font-semibold">
                                          HTTP-01 Challenge
                                          {choices.recommended === 'http-01' &&
                                            ' (Recommended)'}
                                        </div>
                                      </div>
                                      <p className="text-sm text-muted-foreground">
                                        Validates domain ownership by serving a
                                        file over HTTP. Requires port 80 to be
                                        accessible. Best for single domains and
                                        simpler setup.
                                      </p>
                                      <div className="space-y-1 text-sm">
                                        <div className="flex items-center gap-2 text-green-700 dark:text-green-600">
                                          <CheckCircle className="h-4 w-4" />
                                          <span>
                                            Simple setup - just point DNS
                                          </span>
                                        </div>
                                        <div className="flex items-center gap-2 text-green-700 dark:text-green-600">
                                          <CheckCircle className="h-4 w-4" />
                                          <span>Automatic validation</span>
                                        </div>
                                        <div className="flex items-center gap-2 text-green-700 dark:text-green-600">
                                          <CheckCircle className="h-4 w-4" />
                                          <span>
                                            No additional DNS configuration
                                          </span>
                                        </div>
                                      </div>
                                    </div>
                                  </label>
                                </Card>
                              )}

                              <Card
                                className={`relative cursor-pointer border-2 transition-colors ${field.value === 'dns-01' ? 'border-primary' : 'border-border hover:border-primary/50'}`}
                              >
                                <label className="flex cursor-pointer items-start gap-4 p-4">
                                  <RadioGroupItem
                                    value="dns-01"
                                    id="dns-01"
                                    className="mt-1"
                                  />
                                  <div className="flex-1 space-y-2">
                                    <div className="flex items-center gap-2">
                                      <Shield className="h-5 w-5 text-primary" />
                                      <div className="font-semibold">
                                        DNS-01 Challenge
                                        {choices.recommended === 'dns-01' &&
                                          ' (Recommended)'}
                                      </div>
                                    </div>
                                    <p className="text-sm text-muted-foreground">
                                      Validates domain ownership via DNS TXT
                                      records. Required for wildcard
                                      certificates and when port 80 is not
                                      accessible.
                                    </p>
                                    <div className="space-y-1 text-sm">
                                      <div className="flex items-center gap-2 text-green-700 dark:text-green-600">
                                        <CheckCircle className="h-4 w-4" />
                                        <span>
                                          Supports wildcard certificates
                                          (*.example.com)
                                        </span>
                                      </div>
                                      <div className="flex items-center gap-2 text-green-700 dark:text-green-600">
                                        <CheckCircle className="h-4 w-4" />
                                        <span>Works without HTTP access</span>
                                      </div>
                                      <div className="flex items-center gap-2 text-yellow-700 dark:text-yellow-600">
                                        <Info className="h-4 w-4" />
                                        <span>
                                          Requires manual DNS record creation
                                        </span>
                                      </div>
                                    </div>
                                  </div>
                                </label>
                              </Card>
                              {choices.http01Note && (
                                <p className="text-sm text-muted-foreground">
                                  {choices.http01Note}
                                </p>
                              )}
                              {covering.length === 0 &&
                                !connectorsQuery.isLoading && (
                                  <NoTunnelYet
                                    domain={watchedValues.domain ?? ''}
                                  />
                                )}
                            </RadioGroup>
                          </FormControl>
                          <FormMessage />
                        </FormItem>
                      )
                    }}
                  />

                  <div className="flex justify-between gap-2">
                    <Button
                      type="button"
                      variant="outline"
                      onClick={handleBack}
                    >
                      Back
                    </Button>
                    <Button type="button" onClick={handleNext}>
                      Next
                    </Button>
                  </div>
                </div>
              )}

              {/* Step 3: Confirm */}
              {step === 'confirm' && (
                <div className="space-y-4">
                  <div className="flex items-center gap-2 text-sm text-muted-foreground">
                    <div className="flex items-center gap-2">
                      <div className="flex h-6 w-6 items-center justify-center rounded-full bg-muted text-xs font-medium text-muted-foreground">
                        <CheckCircle className="h-4 w-4 text-green-600" />
                      </div>
                      <span>Domain</span>
                    </div>
                    <div className="h-px flex-1 bg-border" />
                    <div className="flex items-center gap-2">
                      <div className="flex h-6 w-6 items-center justify-center rounded-full bg-muted text-xs font-medium text-muted-foreground">
                        <CheckCircle className="h-4 w-4 text-green-600" />
                      </div>
                      <span>HTTPS</span>
                    </div>
                    <div className="h-px flex-1 bg-border" />
                    <div className="flex items-center gap-2">
                      <div className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs font-medium text-primary-foreground">
                        3
                      </div>
                      <span className="font-medium">Confirm</span>
                    </div>
                  </div>

                  {tunnel ? (
                    <TunnelConfirm
                      domain={watchedValues.domain ?? ''}
                      tunnelName={tunnel.name}
                      tunnelDomain={tunnel.domain}
                      publishesAll={tunnel.auto_publish}
                      wildcard={wildcardInput}
                      tooDeep={tooDeep}
                      projects={projectsQuery.data?.projects ?? []}
                      environments={environmentsQuery.data ?? []}
                      project={attachProject}
                      environment={attachEnvironment}
                      onProjectChange={(value) => {
                        setAttachProject(value)
                        setAttachEnvironment('')
                      }}
                      onEnvironmentChange={setAttachEnvironment}
                    />
                  ) : (
                    <>
                      <div className="rounded-lg bg-muted p-4 space-y-3">
                        <h3 className="font-semibold">
                          Review your configuration
                        </h3>
                        <div className="space-y-2">
                          <div className="flex items-center justify-between">
                            <span className="text-sm text-muted-foreground">
                              Domain:
                            </span>
                            <span className="font-medium">
                              {watchedValues.domain}
                            </span>
                          </div>
                          <div className="flex items-center justify-between">
                            <span className="text-sm text-muted-foreground">
                              Challenge type:
                            </span>
                            <span className="font-medium">
                              {watchedValues.challengeType === 'http-01'
                                ? 'HTTP-01 (Automatic)'
                                : 'DNS-01 (Manual)'}
                            </span>
                          </div>
                        </div>
                      </div>

                      {watchedValues.challengeType === 'http-01' ? (
                        <Alert>
                          <Globe className="h-4 w-4" />
                          <AlertDescription>
                            <div className="space-y-2">
                              <p className="font-medium">
                                Next steps (HTTP-01):
                              </p>
                              <ol className="list-decimal list-inside space-y-1 text-sm">
                                <li>
                                  Point your domain&apos;s DNS A record to your
                                  server IP
                                </li>
                                <li>
                                  Ensure port 80 is accessible on your server
                                </li>
                                <li>
                                  The system will automatically validate and
                                  provision the certificate
                                </li>
                              </ol>
                            </div>
                          </AlertDescription>
                        </Alert>
                      ) : (
                        <Alert>
                          <Shield className="h-4 w-4" />
                          <AlertDescription>
                            <div className="space-y-2">
                              <p className="font-medium">
                                Next steps (DNS-01):
                              </p>
                              <ol className="list-decimal list-inside space-y-1 text-sm">
                                <li>
                                  After creation, you&apos;ll receive a DNS TXT
                                  record to add
                                </li>
                                <li>
                                  Add the TXT record to your domain&apos;s DNS
                                  settings
                                </li>
                                <li>
                                  Wait for DNS propagation (up to 24 hours)
                                </li>
                                <li>
                                  Complete the DNS challenge to provision the
                                  certificate
                                </li>
                              </ol>
                            </div>
                          </AlertDescription>
                        </Alert>
                      )}
                    </>
                  )}

                  <div className="flex justify-between gap-2">
                    <Button
                      type="button"
                      variant="outline"
                      onClick={handleBack}
                    >
                      Back
                    </Button>
                    <Button type="submit" disabled={submitting}>
                      {tunnel
                        ? submitting
                          ? 'Adding…'
                          : 'Add domain'
                        : createDomain.isPending
                          ? 'Creating domain...'
                          : 'Create domain & start provisioning'}
                    </Button>
                  </div>
                </div>
              )}
            </form>
          </Form>
        </div>
      </div>
    </div>
  )
}

/**
 * The tunnel way in, before any tunnel covers the domain: what it needs and
 * where to set it up, so the choice is not silently missing.
 */
function NoTunnelYet({ domain }: { domain: string }) {
  return (
    <Card className="border-2 border-dashed">
      <div className="flex items-start gap-4 p-4">
        <Cloud className="mt-1 h-5 w-5 text-muted-foreground" />
        <div className="flex-1 space-y-3">
          <div className="space-y-1">
            <div className="font-semibold">Through a Cloudflare Tunnel</div>
            <p className="text-sm text-muted-foreground">
              No tunnel covers {domain || 'this domain'} yet. With one, a worker
              connects out to Cloudflare: no public IP or open port, and
              Cloudflare serves HTTPS, so there is no certificate to issue here.
            </p>
          </div>
          <div className="space-y-1 text-sm">
            <p className="font-medium">You need</p>
            <ul className="list-disc space-y-0.5 pl-5 text-muted-foreground">
              <li>The domain on Cloudflare (the free plan works)</li>
              <li>A worker node to run the tunnel</li>
              <li>A Cloudflare API token with these permissions:</li>
            </ul>
          </div>
          <CloudflareTokenChecklist />
          <div className="flex flex-wrap items-center gap-3">
            <Button type="button" size="sm" asChild>
              <Link to={ingressSetupUrl({ via: 'tunnel' })}>
                Set up a Cloudflare Tunnel
              </Link>
            </Button>
            <span className="text-xs text-muted-foreground">
              Then add {domain || 'the domain'} again: it is offered here.
            </span>
          </div>
        </div>
      </div>
    </Card>
  )
}

function TunnelConfirm({
  domain,
  tunnelName,
  tunnelDomain,
  publishesAll,
  wildcard,
  tooDeep,
  projects,
  environments,
  project,
  environment,
  onProjectChange,
  onEnvironmentChange,
}: {
  domain: string
  tunnelName: string
  tunnelDomain: string
  publishesAll: boolean
  wildcard: boolean
  tooDeep: boolean
  projects: { id: number; name: string }[]
  environments: { id: number; name: string }[]
  project: string
  environment: string
  onProjectChange: (value: string) => void
  onEnvironmentChange: (value: string) => void
}) {
  return (
    <div className="space-y-4">
      <div className="rounded-lg bg-muted p-4 space-y-2">
        <h3 className="font-semibold">Review</h3>
        <div className="flex items-center justify-between gap-4 text-sm">
          <span className="text-muted-foreground">Domain</span>
          <span className="font-medium">{domain}</span>
        </div>
        <div className="flex items-center justify-between gap-4 text-sm">
          <span className="text-muted-foreground">Served through</span>
          <span className="font-medium">Tunnel {tunnelName}</span>
        </div>
        <div className="flex items-center justify-between gap-4 text-sm">
          <span className="text-muted-foreground">HTTPS</span>
          <span className="font-medium">From the tunnel&apos;s provider</span>
        </div>
      </div>

      {tooDeep && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription>
            Cloudflare&apos;s free certificate covers {tunnelDomain} and names
            one level below it, such as app.{tunnelDomain}. Visitors to {domain}{' '}
            would get a certificate error unless the zone has an advanced
            certificate that covers it.
          </AlertDescription>
        </Alert>
      )}

      {wildcard ? (
        <Alert>
          <Cloud className="h-4 w-4" />
          <AlertDescription>
            {publishesAll
              ? `Tunnel ${tunnelName} already publishes every app under ${tunnelDomain}. Add names under it to environments and each goes live on its own.`
              : `Tunnel ${tunnelName} will publish every app under ${tunnelDomain}, instead of only the names picked for it.`}
          </AlertDescription>
        </Alert>
      ) : (
        <div className="space-y-2">
          <p className="text-sm font-medium">Serve it from (optional)</p>
          <p className="text-sm text-muted-foreground">
            The tunnel publishes it once an environment serves it. Pick one now,
            or add it later from the project&apos;s domain settings.
          </p>
          <div className="grid gap-2 sm:grid-cols-2">
            <Select value={project} onValueChange={onProjectChange}>
              <SelectTrigger aria-label="Project">
                <SelectValue placeholder="Project" />
              </SelectTrigger>
              <SelectContent>
                {projects.map((p) => (
                  <SelectItem key={p.id} value={String(p.id)}>
                    {p.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select
              value={environment}
              onValueChange={onEnvironmentChange}
              disabled={!project}
            >
              <SelectTrigger aria-label="Environment">
                <SelectValue placeholder="Environment" />
              </SelectTrigger>
              <SelectContent>
                {environments.map((e) => (
                  <SelectItem key={e.id} value={String(e.id)}>
                    {e.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>
      )}
    </div>
  )
}
