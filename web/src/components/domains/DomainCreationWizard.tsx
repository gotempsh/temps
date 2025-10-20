import { createDomainMutation } from '@/api/client/@tanstack/react-query.gen'
import { DomainResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
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
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { CheckCircle, CopyIcon, Globe, Info, Shield } from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const domainWizardSchema = z.object({
  domain: z
    .string()
    .min(1, 'Domain is required')
    .regex(
      /^(?:[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$/,
      'Invalid domain format'
    ),
  challengeType: z.enum(['http-01', 'dns-01']),
})

type DomainWizardFormData = z.infer<typeof domainWizardSchema>

interface DomainCreationWizardProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  onSuccess: () => void
}

export function DomainCreationWizard({
  open,
  onOpenChange,
  onSuccess,
}: DomainCreationWizardProps) {
  const [step, setStep] = useState<
    'domain' | 'challenge' | 'confirm' | 'dns-instructions'
  >('domain')
  const [createdDomain, setCreatedDomain] = useState<DomainResponse | null>(
    null
  )

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
      const challengeType = form.getValues('challengeType')

      if (
        challengeType === 'dns-01' &&
        data.dns_challenge_token &&
        data.dns_challenge_value
      ) {
        // For DNS challenge, show the DNS instructions step
        setCreatedDomain(data)
        setStep('dns-instructions')
      } else {
        // For HTTP challenge, close and show success
        toast.success(
          'Domain added successfully! Certificate provisioning has started.'
        )
        handleClose()
        onSuccess()
      }
    },
  })

  const handleClose = () => {
    setStep('domain')
    setCreatedDomain(null)
    form.reset()
    onOpenChange(false)
  }

  const handleFinish = () => {
    handleClose()
    onSuccess()
  }

  const handleNext = () => {
    if (step === 'domain') {
      form.trigger('domain').then((valid) => {
        if (valid) setStep('challenge')
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
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Add New Domain & Certificate</DialogTitle>
        </DialogHeader>

        <Form {...form}>
          <form
            onSubmit={form.handleSubmit(handleSubmit)}
            className="space-y-6"
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
                        <Input {...field} placeholder="example.com" autoFocus />
                      </FormControl>
                      <FormDescription>
                        Enter your custom domain (e.g., example.com or
                        app.example.com)
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                <Alert>
                  <Info className="h-4 w-4" />
                  <AlertDescription>
                    You&apos;ll need to point your domain&apos;s DNS records to
                    your server after creation. We&apos;ll provide detailed
                    instructions in the next steps.
                  </AlertDescription>
                </Alert>

                <div className="flex justify-end gap-2">
                  <Button type="button" variant="outline" onClick={handleClose}>
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
                  render={({ field }) => (
                    <FormItem className="space-y-4">
                      <FormLabel>SSL Certificate Challenge Type</FormLabel>
                      <FormDescription>
                        Choose how you want to verify domain ownership for
                        Let&apos;s Encrypt
                      </FormDescription>
                      <FormControl>
                        <RadioGroup
                          onValueChange={field.onChange}
                          value={field.value}
                          className="grid gap-4"
                        >
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
                                  Validates domain ownership by serving a file
                                  over HTTP. Requires port 80 to be accessible.
                                  Best for single domains and simpler setup.
                                </p>
                                <div className="space-y-1 text-sm">
                                  <div className="flex items-center gap-2 text-green-600">
                                    <CheckCircle className="h-4 w-4" />
                                    <span>Simple setup - just point DNS</span>
                                  </div>
                                  <div className="flex items-center gap-2 text-green-600">
                                    <CheckCircle className="h-4 w-4" />
                                    <span>Automatic validation</span>
                                  </div>
                                  <div className="flex items-center gap-2 text-green-600">
                                    <CheckCircle className="h-4 w-4" />
                                    <span>No additional DNS configuration</span>
                                  </div>
                                </div>
                              </div>
                            </label>
                          </Card>

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
                                  records. Required for wildcard certificates
                                  and when port 80 is not accessible.
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
                  )}
                />

                <div className="flex justify-between gap-2">
                  <Button type="button" variant="outline" onClick={handleBack}>
                    Back
                  </Button>
                  <Button type="button" onClick={handleNext}>
                    Next
                  </Button>
                </div>
              </div>
            )}

            {/* Step 3: Confirm */}
            {step === 'confirm' && !createdDomain && (
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
                  <h3 className="font-semibold">Review Your Configuration</h3>
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
                            Point your domain&apos;s DNS A record to your server
                            IP
                          </li>
                          <li>Ensure port 80 is accessible on your server</li>
                          <li>
                            The system will automatically validate and provision
                            the certificate
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
                            After creation, you&apos;ll receive a DNS TXT record
                            to add
                          </li>
                          <li>
                            Add the TXT record to your domain&apos;s DNS
                            settings
                          </li>
                          <li>Wait for DNS propagation (up to 24 hours)</li>
                          <li>
                            Click &quot;Complete DNS Challenge&quot; to verify
                            and provision the certificate
                          </li>
                        </ol>
                      </div>
                    </AlertDescription>
                  </Alert>
                )}

                <div className="flex justify-between gap-2">
                  <Button type="button" variant="outline" onClick={handleBack}>
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

            {/* Step 4: DNS Instructions (DNS-01 only) */}
            {step === 'dns-instructions' && createdDomain && (
              <div className="space-y-4">
                <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/10">
                  <CheckCircle className="h-4 w-4 text-green-600" />
                  <AlertTitle className="text-green-900 dark:text-green-100">
                    Domain Created Successfully!
                  </AlertTitle>
                  <AlertDescription className="text-green-800 dark:text-green-200">
                    Your domain <strong>{createdDomain.domain}</strong> has been
                    created. Now complete the DNS challenge to provision the SSL
                    certificate.
                  </AlertDescription>
                </Alert>

                <Alert>
                  <Shield className="h-4 w-4" />
                  <AlertTitle>DNS Challenge Required</AlertTitle>
                  <AlertDescription>
                    <div className="mt-2 space-y-4">
                      <p className="text-sm">
                        Add the following DNS TXT record to your domain&apos;s
                        DNS settings to verify ownership:
                      </p>

                      <div className="space-y-3">
                        <div className="space-y-2">
                          <div className="flex items-center justify-between">
                            <span className="text-sm font-medium">
                              Record Type
                            </span>
                          </div>
                          <code className="relative block rounded bg-muted px-3 py-2 font-mono text-sm">
                            TXT
                          </code>
                        </div>

                        <div className="space-y-2">
                          <div className="flex items-center justify-between">
                            <span className="text-sm font-medium">
                              Record Name
                            </span>
                            <Button
                              variant="ghost"
                              size="sm"
                              className="h-8"
                              onClick={() => {
                                if (createdDomain.dns_challenge_token) {
                                  navigator.clipboard.writeText(
                                    createdDomain.dns_challenge_token
                                  )
                                  toast.success('Copied to clipboard')
                                }
                              }}
                            >
                              <CopyIcon className="h-3 w-3 mr-2" />
                              Copy
                            </Button>
                          </div>
                          <code className="relative block rounded bg-muted px-3 py-2 font-mono text-sm break-all">
                            {createdDomain.dns_challenge_token}
                          </code>
                        </div>

                        <div className="space-y-2">
                          <div className="flex items-center justify-between">
                            <span className="text-sm font-medium">
                              Record Value
                            </span>
                            <Button
                              variant="ghost"
                              size="sm"
                              className="h-8"
                              onClick={() => {
                                if (createdDomain.dns_challenge_value) {
                                  navigator.clipboard.writeText(
                                    createdDomain.dns_challenge_value
                                  )
                                  toast.success('Copied to clipboard')
                                }
                              }}
                            >
                              <CopyIcon className="h-3 w-3 mr-2" />
                              Copy
                            </Button>
                          </div>
                          <code className="relative block rounded bg-muted px-3 py-2 font-mono text-sm break-all">
                            {createdDomain.dns_challenge_value}
                          </code>
                        </div>
                      </div>

                      <div className="rounded-lg bg-blue-50 dark:bg-blue-950/20 p-3 space-y-2">
                        <p className="text-sm font-medium text-blue-900 dark:text-blue-100">
                          Next Steps:
                        </p>
                        <ol className="list-decimal list-inside space-y-1 text-sm text-blue-800 dark:text-blue-200">
                          <li>
                            Add the TXT record to your DNS provider (e.g.,
                            Cloudflare, Route53, Namecheap)
                          </li>
                          <li>
                            Wait for DNS propagation (can take up to 24 hours,
                            usually 5-15 minutes)
                          </li>
                          <li>
                            Return to the Domains page and click &quot;Complete
                            DNS Challenge&quot; on your domain
                          </li>
                          <li>
                            The system will verify the DNS record and provision
                            the SSL certificate
                          </li>
                        </ol>
                      </div>

                      <p className="text-sm text-muted-foreground">
                        <Info className="h-4 w-4 inline mr-1" />
                        You can close this dialog and complete the DNS challenge
                        later from the Domains page.
                      </p>
                    </div>
                  </AlertDescription>
                </Alert>

                <div className="flex justify-end gap-2">
                  <Button type="button" onClick={handleFinish}>
                    Got it, I&apos;ll configure DNS
                  </Button>
                </div>
              </div>
            )}
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
