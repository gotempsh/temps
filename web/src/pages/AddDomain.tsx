import { createDomainMutation } from '@/api/client/@tanstack/react-query.gen'
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
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { CheckCircle, Globe, Info, Shield, ArrowLeft } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'

const domainWizardSchema = z.object({
  domain: z
    .string()
    .min(1, 'Domain is required')
    .regex(
      /^(\*\.)?[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?)*\.[a-zA-Z]{2,}$/,
      'Invalid domain format (e.g., example.com or *.example.com)'
    ),
  challengeType: z.enum(['http-01', 'dns-01']),
})

type DomainWizardFormData = z.infer<typeof domainWizardSchema>

export function AddDomain() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const [step, setStep] = useState<'domain' | 'challenge' | 'confirm'>('domain')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Domains', href: '/domains' },
      { label: 'Add Domain' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Add Domain')

  const form = useForm<DomainWizardFormData>({
    resolver: zodResolver(domainWizardSchema),
    defaultValues: {
      domain: '',
      challengeType: 'http-01',
    },
  })

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

  const handleFinish = () => {
    navigate('/domains')
  }

  const handleNext = () => {
    if (step === 'domain') {
      form.trigger('domain').then((valid) => {
        if (valid) {
          // Auto-select DNS-01 for wildcard domains
          const domain = form.getValues('domain')
          if (domain.startsWith('*.')) {
            form.setValue('challengeType', 'dns-01')
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

  const handleSubmit = async (data: DomainWizardFormData) => {
    try {
      await createDomain.mutateAsync({
        body: {
          domain: data.domain,
          challenge_type: data.challengeType,
        },
      })
    } catch (error) {
      // Error handled in onError
    }
  }

  const watchedValues = form.watch()

  return (
    <div className="flex-1 overflow-auto">
      <div className="max-w-3xl mx-auto space-y-6">
        {/* Header */}
        <div className="flex items-center gap-4">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate('/domains')}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h1 className="text-2xl font-bold">
              Add New Domain & SSL Certificate
            </h1>
            <p className="text-sm text-muted-foreground mt-1">
              Configure a custom domain and provision an SSL certificate
            </p>
          </div>
        </div>

        <Card>
          <div className="p-6">
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
                        <span>Challenge</span>
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
                          <FormLabel>Domain Name</FormLabel>
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
                        You'll need to point your domain's DNS records to your
                        server after creation. We'll provide detailed
                        instructions in the next steps.
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
                        <span className="font-medium">Challenge</span>
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
                        const isWildcard =
                          watchedValues.domain?.startsWith('*.')
                        return (
                          <FormItem className="space-y-4">
                            <FormLabel>
                              SSL Certificate Challenge Type
                            </FormLabel>
                            <FormDescription>
                              {isWildcard
                                ? 'Wildcard domains require DNS-01 challenge'
                                : "Choose how you want to verify domain ownership for Let's Encrypt"}
                            </FormDescription>
                            <FormControl>
                              <RadioGroup
                                onValueChange={field.onChange}
                                value={field.value}
                                className="grid gap-4"
                              >
                                {!isWildcard && (
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
                                            HTTP-01 Challenge (Recommended)
                                          </div>
                                        </div>
                                        <p className="text-sm text-muted-foreground">
                                          Validates domain ownership by serving
                                          a file over HTTP. Requires port 80 to
                                          be accessible. Best for single domains
                                          and simpler setup.
                                        </p>
                                        <div className="space-y-1 text-sm">
                                          <div className="flex items-center gap-2 text-green-600">
                                            <CheckCircle className="h-4 w-4" />
                                            <span>
                                              Simple setup - just point DNS
                                            </span>
                                          </div>
                                          <div className="flex items-center gap-2 text-green-600">
                                            <CheckCircle className="h-4 w-4" />
                                            <span>Automatic validation</span>
                                          </div>
                                          <div className="flex items-center gap-2 text-green-600">
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
                                        </div>
                                      </div>
                                      <p className="text-sm text-muted-foreground">
                                        Validates domain ownership via DNS TXT
                                        records. Required for wildcard
                                        certificates and when port 80 is not
                                        accessible.
                                      </p>
                                      <div className="space-y-1 text-sm">
                                        <div className="flex items-center gap-2 text-green-600">
                                          <CheckCircle className="h-4 w-4" />
                                          <span>
                                            Supports wildcard certificates
                                            (*.example.com)
                                          </span>
                                        </div>
                                        <div className="flex items-center gap-2 text-green-600">
                                          <CheckCircle className="h-4 w-4" />
                                          <span>Works without HTTP access</span>
                                        </div>
                                        <div className="flex items-center gap-2 text-yellow-600">
                                          <Info className="h-4 w-4" />
                                          <span>
                                            Requires manual DNS record creation
                                          </span>
                                        </div>
                                      </div>
                                    </div>
                                  </label>
                                </Card>
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
                        <span>Challenge</span>
                      </div>
                      <div className="h-px flex-1 bg-border" />
                      <div className="flex items-center gap-2">
                        <div className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs font-medium text-primary-foreground">
                          3
                        </div>
                        <span className="font-medium">Confirm</span>
                      </div>
                    </div>

                    <div className="rounded-lg bg-muted p-4 space-y-3">
                      <h3 className="font-semibold">
                        Review Your Configuration
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
                            Challenge Type:
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
                            <p className="font-medium">Next Steps (HTTP-01):</p>
                            <ol className="list-decimal list-inside space-y-1 text-sm">
                              <li>
                                Point your domain's DNS A record to your server
                                IP
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
                            <p className="font-medium">Next Steps (DNS-01):</p>
                            <ol className="list-decimal list-inside space-y-1 text-sm">
                              <li>
                                After creation, you'll receive a DNS TXT record
                                to add
                              </li>
                              <li>
                                Add the TXT record to your domain's DNS settings
                              </li>
                              <li>Wait for DNS propagation (up to 24 hours)</li>
                              <li>
                                Complete the DNS challenge to provision the
                                certificate
                              </li>
                            </ol>
                          </div>
                        </AlertDescription>
                      </Alert>
                    )}

                    <div className="flex justify-between gap-2">
                      <Button
                        type="button"
                        variant="outline"
                        onClick={handleBack}
                      >
                        Back
                      </Button>
                      <Button type="submit" disabled={createDomain.isPending}>
                        {createDomain.isPending
                          ? 'Creating Domain...'
                          : 'Create Domain & Start Provisioning'}
                      </Button>
                    </div>
                  </div>
                )}
              </form>
            </Form>
          </div>
        </Card>
      </div>
    </div>
  )
}
