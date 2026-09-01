// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import {
  listEmailProviders as listProviders2,
  testProvider,
  updateEmailProvider as updateProvider2,
  type EmailProviderResponse,
  type TestEmailRequest as SdkTestEmailRequest,
  type TestEmailResponse as SdkTestEmailResponse,
  type UpdateEmailProviderRequest,
} from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
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
import { Skeleton } from '@/components/ui/skeleton'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { EllipsisVertical, Loader2, Mail, Send, Server } from 'lucide-react'
import { AWSIcon } from '@/components/icons/AWSIcon'
import { ScalewayIcon } from '@/components/icons/ScalewayIcon'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import {
  awsRegions,
  deleteEmailProvider,
  problemMessage,
  providerTypeLabel,
  readMaskedCreds,
  scalewayRegions,
} from './sharedUtils'

// Types for email providers — alias over SDK response
type EmailProvider = EmailProviderResponse
type TestEmailResponse = SdkTestEmailResponse
type TestEmailRequest = SdkTestEmailRequest

async function listEmailProviders(): Promise<EmailProvider[]> {
  const response = await listProviders2()
  if (response.error) {
    throw new Error(
      problemMessage(response.error, 'Failed to fetch email providers')
    )
  }
  return response.data ?? []
}

async function updateEmailProviderApi(
  id: number,
  body: UpdateEmailProviderRequest
): Promise<EmailProvider> {
  const response = await updateProvider2({ path: { id }, body })
  if (response.error || !response.data) {
    throw new Error(
      problemMessage(response.error, 'Failed to update email provider')
    )
  }
  return response.data
}

async function testEmailProvider(
  id: number,
  request: TestEmailRequest
): Promise<TestEmailResponse> {
  const response = await testProvider({ path: { id }, body: request })
  if (response.error || !response.data) {
    throw new Error(problemMessage(response.error, 'Failed to send test email'))
  }
  return response.data
}

// Test email form schema
const testEmailSchema = z.object({
  from: z.string().email('Please enter a valid email address'),
  from_name: z.string().optional(),
})

type TestEmailFormData = z.infer<typeof testEmailSchema>

function ProviderIcon({ type }: { type: 'ses' | 'scaleway' | 'smtp' }) {
  if (type === 'ses') {
    return <AWSIcon className="h-5 w-5 text-[#FF9900]" />
  }
  if (type === 'scaleway') {
    return <ScalewayIcon className="h-5 w-5 text-[#4F0599]" />
  }
  return <Server className="h-5 w-5 text-slate-600 dark:text-slate-300" />
}

export function TestEmailDialog({
  open,
  onOpenChange,
  providerId,
  onSuccess,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  providerId: number | null
  onSuccess: () => void
}) {
  const form = useForm<TestEmailFormData>({
    resolver: zodResolver(testEmailSchema),
    defaultValues: {
      from: '',
      from_name: '',
    },
  })

  const testMutation = useMutation({
    mutationFn: (data: TestEmailFormData) =>
      testEmailProvider(providerId!, data),
    onSuccess: (data) => {
      if (data.success) {
        toast.success('Test email sent successfully!', {
          description: `A test email was sent to ${data.sent_to}. Please check your inbox.`,
        })
        onOpenChange(false)
        form.reset()
        onSuccess()
      } else {
        toast.error('Test email failed', {
          description: data.error || 'Unknown error occurred',
        })
      }
    },
    onError: (error: Error) => {
      toast.error('Failed to send test email', {
        description: error.message,
      })
    },
  })

  const onSubmit = (data: TestEmailFormData) => {
    testMutation.mutate(data)
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Send Test Email</DialogTitle>
          <DialogDescription>
            Enter the sender details to send a test email. The &quot;From&quot;
            address must be verified with your email provider.
          </DialogDescription>
        </DialogHeader>

        <Form {...form}>
          <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
            <FormField
              control={form.control}
              name="from"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>From Email Address</FormLabel>
                  <FormControl>
                    <Input placeholder="noreply@yourdomain.com" {...field} />
                  </FormControl>
                  <FormDescription>
                    The email address to send from. Must be verified with AWS
                    SES or your domain must be verified with Scaleway.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            <FormField
              control={form.control}
              name="from_name"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>From Name (Optional)</FormLabel>
                  <FormControl>
                    <Input placeholder="My Application" {...field} />
                  </FormControl>
                  <FormDescription>
                    The display name shown in the recipient&apos;s email client.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />

            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={testMutation.isPending}>
                {testMutation.isPending && (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                )}
                Send Test Email
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}

function ProviderCard({
  provider,
  onDelete,
  onTestClick,
  onEditClick,
}: {
  provider: EmailProvider
  onDelete: (id: number) => void
  onTestClick: (id: number) => void
  onEditClick: (provider: EmailProvider) => void
}) {
  const [isDeleting, setIsDeleting] = useState(false)
  const navigate = useNavigate()

  const handleDelete = () => {
    setIsDeleting(true)
    try {
      onDelete(provider.id)
    } finally {
      setIsDeleting(false)
    }
  }

  return (
    <Card
      onClick={() => navigate(`/email/providers/${provider.id}`)}
      className="cursor-pointer transition-colors hover:bg-muted/40"
    >
      <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
        <div className="flex items-center gap-3">
          <ProviderIcon type={provider.provider_type} />
          <div>
            <CardTitle className="text-base font-medium leading-none">
              {provider.name}
            </CardTitle>
            <p className="text-xs text-muted-foreground mt-1">
              {providerTypeLabel(provider.provider_type)}
            </p>
          </div>
        </div>
        <div
          className="flex items-center gap-2"
          onClick={(e) => e.stopPropagation()}
        >
          <Badge variant={provider.is_active ? 'default' : 'secondary'}>
            {provider.is_active ? 'Active' : 'Inactive'}
          </Badge>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon" className="h-8 w-8">
                <EllipsisVertical className="h-4 w-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onClick={() => onTestClick(provider.id)}>
                <Send className="mr-2 h-4 w-4" />
                Send Test Email
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => onEditClick(provider)}>
                Edit
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                className="text-destructive"
                onClick={handleDelete}
                disabled={isDeleting}
              >
                {isDeleting ? 'Deleting...' : 'Delete'}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </CardHeader>
      <CardContent>
        <div className="space-y-2 text-sm">
          <div className="flex justify-between">
            <span className="text-muted-foreground">Region</span>
            <span className="font-mono">{provider.region}</span>
          </div>
          <div className="flex justify-between">
            <span className="text-muted-foreground">Created</span>
            <span>
              {formatDistanceToNow(new Date(provider.created_at), {
                addSuffix: true,
              })}
            </span>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}

function LoadingSkeleton() {
  return (
    <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
      {[1, 2, 3].map((i) => (
        <Card key={i}>
          <CardHeader className="pb-2">
            <div className="flex items-center gap-3">
              <Skeleton className="h-10 w-10 rounded-full" />
              <div className="space-y-2">
                <Skeleton className="h-4 w-24" />
                <Skeleton className="h-3 w-16" />
              </div>
            </div>
          </CardHeader>
          <CardContent>
            <div className="space-y-2">
              <Skeleton className="h-4 w-full" />
              <Skeleton className="h-4 w-3/4" />
            </div>
          </CardContent>
        </Card>
      ))}
    </div>
  )
}

// ============================================================================
// Edit dialog
// ============================================================================
//
// Editing a provider is a partial update: any credential field left blank
// preserves the stored secret. The provider_type is locked because the
// stored credentials format is fixed at creation time — to switch providers
// the user must delete and recreate.

const editProviderSchema = z
  .object({
    name: z.string().min(1, 'Name is required'),
    region: z.string().min(1, 'Region is required'),
    is_active: z.boolean(),
    // SES — leave blank to keep current
    sns_topic_arn: z.string().optional(),
    access_key_id: z.string().optional(),
    secret_access_key: z.string().optional(),
    // Scaleway — leave blank to keep current
    api_key: z.string().optional(),
    project_id: z.string().optional(),
    // SMTP — host/port/encryption/accept_invalid_certs are visible (not secret)
    // and must always be present. username/password are sensitive and may be
    // left blank to preserve the stored value.
    smtp_host: z.string().optional(),
    smtp_port: z.number().int().min(1).max(65535).optional(),
    smtp_username: z.string().optional(),
    smtp_password: z.string().optional(),
    smtp_encryption: z.enum(['starttls', 'tls', 'none']).optional(),
    smtp_accept_invalid_certs: z.boolean().optional(),
  })
  .superRefine((data, ctx) => {
    // For SES/Scaleway: secret without identifier (or vice versa) makes no
    // sense — both must be supplied together when rotating, or neither.
    if (
      (data.access_key_id && !data.secret_access_key) ||
      (!data.access_key_id && data.secret_access_key)
    ) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message:
          'Provide both access key ID and secret access key, or leave both blank',
        path: ['secret_access_key'],
      })
    }
    if (
      (data.api_key && !data.project_id) ||
      (!data.api_key && data.project_id)
    ) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message: 'Provide both API key and project ID, or leave both blank',
        path: ['api_key'],
      })
    }
  })

type EditProviderFormData = z.infer<typeof editProviderSchema>

interface EditProviderDialogProps {
  provider: EmailProvider | null
  open: boolean
  onOpenChange: (open: boolean) => void
  onSuccess: () => void
}

export function EditProviderDialog({
  provider,
  open,
  onOpenChange,
  onSuccess,
}: EditProviderDialogProps) {
  const queryClient = useQueryClient()

  const form = useForm<EditProviderFormData>({
    resolver: zodResolver(editProviderSchema),
    defaultValues: {
      name: '',
      region: '',
      is_active: true,
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

  // Reset form whenever a different provider is loaded into the dialog.
  // We use `useEffect` rather than `defaultValues` because the dialog
  // is mounted once and the provider can change while it's open.
  useEffect(() => {
    if (!provider) return
    const masked = readMaskedCreds(provider.credentials)
    form.reset({
      name: provider.name,
      region: provider.region,
      is_active: provider.is_active,
      sns_topic_arn: provider.sns_topic_arn ?? '',
      access_key_id: '',
      secret_access_key: '',
      api_key: '',
      project_id: '',
      smtp_host: masked.host ?? '',
      smtp_port: masked.port ?? 587,
      smtp_username: '',
      smtp_password: '',
      smtp_encryption: masked.encryption ?? 'starttls',
      smtp_accept_invalid_certs: masked.accept_invalid_certs ?? false,
    })
  }, [provider, form])

  const updateMutation = useMutation({
    mutationFn: async (data: EditProviderFormData): Promise<EmailProvider> => {
      if (!provider) throw new Error('No provider selected')

      const body: UpdateEmailProviderRequest = {}
      // Only include scalars that actually changed, so audit logs stay precise.
      if (data.name !== provider.name) body.name = data.name
      if (data.region !== provider.region) body.region = data.region
      if (data.is_active !== provider.is_active) body.is_active = data.is_active

      if (provider.provider_type === 'ses') {
        if ((data.sns_topic_arn ?? '') !== (provider.sns_topic_arn ?? '')) {
          body.sns_topic_arn = data.sns_topic_arn || null
        }
        if (data.access_key_id && data.secret_access_key) {
          body.ses_credentials = {
            access_key_id: data.access_key_id,
            secret_access_key: data.secret_access_key,
          }
        }
      } else if (provider.provider_type === 'scaleway') {
        if (data.api_key && data.project_id) {
          body.scaleway_credentials = {
            api_key: data.api_key,
            project_id: data.project_id,
          }
        }
      } else if (provider.provider_type === 'smtp') {
        // Non-secret SMTP fields are always sent when they differ. If any of
        // host/port/encryption/accept_invalid_certs changes, we have to
        // resend the whole credential block (the backend re-encrypts on
        // every credentials update), so password is sent too — using the
        // freshly-typed value if present, otherwise omitted to fall back
        // to the previously-stored secret on the backend.
        const masked = readMaskedCreds(provider.credentials)
        const hostChanged = (data.smtp_host ?? '') !== (masked.host ?? '')
        const portChanged = (data.smtp_port ?? 0) !== (masked.port ?? 0)
        const encChanged =
          (data.smtp_encryption ?? 'starttls') !==
          (masked.encryption ?? 'starttls')
        const certsChanged =
          (data.smtp_accept_invalid_certs ?? false) !==
          (masked.accept_invalid_certs ?? false)
        const passwordTyped = !!data.smtp_password
        const usernameTyped = !!data.smtp_username

        if (
          hostChanged ||
          portChanged ||
          encChanged ||
          certsChanged ||
          passwordTyped ||
          usernameTyped
        ) {
          if (!data.smtp_host) {
            throw new Error('SMTP host is required')
          }
          if (!data.smtp_port) {
            throw new Error('SMTP port is required')
          }
          if (passwordTyped && !usernameTyped) {
            throw new Error('Username is required when setting a new password')
          }
          body.smtp_credentials = {
            host: data.smtp_host,
            port: data.smtp_port,
            username: usernameTyped ? data.smtp_username : undefined,
            password: passwordTyped ? data.smtp_password : undefined,
            encryption: data.smtp_encryption ?? 'starttls',
            accept_invalid_certs: data.smtp_accept_invalid_certs ?? false,
          }
        }
      }

      // If nothing changed, short-circuit — no point pinging the server.
      if (Object.keys(body).length === 0) {
        return provider
      }

      return updateEmailProviderApi(provider.id, body)
    },
    onSuccess: () => {
      toast.success('Email provider updated')
      queryClient.invalidateQueries({ queryKey: ['email-providers'] })
      onOpenChange(false)
      onSuccess()
    },
    onError: (error: Error) => {
      toast.error('Failed to update provider', {
        description: error.message,
      })
    },
  })

  const onSubmit = (data: EditProviderFormData) => {
    updateMutation.mutate(data)
  }

  const providerType = provider?.provider_type
  const regions =
    providerType === 'ses'
      ? awsRegions
      : providerType === 'scaleway'
        ? scalewayRegions
        : []

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>Edit Email Provider</DialogTitle>
          <DialogDescription>
            Update the provider configuration. Leave secret fields blank to keep
            the stored credentials unchanged. The provider type cannot be
            changed.
          </DialogDescription>
        </DialogHeader>

        {provider && (
          <Form {...form}>
            <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
              {/* Read-only provider type indicator */}
              <div className="flex items-center gap-3 rounded-md border border-border bg-muted/40 px-3 py-2 text-sm">
                <ProviderIcon type={provider.provider_type} />
                <span className="font-medium">
                  {providerTypeLabel(provider.provider_type)}
                </span>
                <span className="text-muted-foreground">
                  &middot; provider type is immutable
                </span>
              </div>

              <FormField
                control={form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input placeholder="My Email Provider" {...field} />
                    </FormControl>
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
                            <SelectItem key={region.value} value={region.value}>
                              <div className="flex items-center justify-between gap-4 w-full">
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
                    name="sns_topic_arn"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>SNS Topic ARN</FormLabel>
                        <FormControl>
                          <Input
                            placeholder="arn:aws:sns:us-east-1:123456789012:temps-events"
                            autoComplete="off"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          Exact SNS topic authorized to send SES delivery,
                          bounce, and complaint events for this provider. To set
                          this up automatically, close this dialog and open the
                          provider&apos;s detail page.
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <FormField
                    control={form.control}
                    name="access_key_id"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Access Key ID</FormLabel>
                        <FormControl>
                          <Input
                            placeholder="Leave blank to keep current"
                            autoComplete="off"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          To rotate credentials, enter both the new access key
                          ID and the new secret. Leave both blank to keep the
                          existing credentials.
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
                            placeholder="Leave blank to keep current"
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
                            placeholder="Leave blank to keep current"
                            autoComplete="new-password"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          To rotate credentials, enter both the new API key and
                          the new project ID. Leave both blank to keep the
                          existing values.
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
                            placeholder="Leave blank to keep current"
                            {...field}
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </>
              )}

              {providerType === 'smtp' && (
                <>
                  <div className="grid grid-cols-1 sm:grid-cols-[1fr_120px] gap-4">
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
                                field.onChange(v === '' ? undefined : Number(v))
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
                          onValueChange={field.onChange}
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
                        <FormLabel>Username</FormLabel>
                        <FormControl>
                          <Input
                            placeholder="Leave blank to keep current"
                            autoComplete="off"
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          Required when setting a new password. Leave blank to
                          keep the stored value.
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
                            placeholder="Leave blank to keep current"
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

              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => onOpenChange(false)}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={updateMutation.isPending}>
                  {updateMutation.isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Save changes
                </Button>
              </DialogFooter>
            </form>
          </Form>
        )}
      </DialogContent>
    </Dialog>
  )
}

export function EmailProvidersManagement() {
  const [isTestDialogOpen, setIsTestDialogOpen] = useState(false)
  const [testingProviderId, setTestingProviderId] = useState<number | null>(
    null
  )
  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)
  const [editingProvider, setEditingProvider] = useState<EmailProvider | null>(
    null
  )
  const queryClient = useQueryClient()

  const { data: providers, isLoading } = useQuery({
    queryKey: ['email-providers'],
    queryFn: listEmailProviders,
  })

  const deleteMutation = useMutation({
    mutationFn: deleteEmailProvider,
    onSuccess: () => {
      toast.success('Email provider deleted successfully')
      queryClient.invalidateQueries({ queryKey: ['email-providers'] })
    },
    onError: (error: Error) => {
      toast.error('Failed to delete provider', {
        description: error.message,
      })
    },
  })

  const handleDelete = (id: number) => {
    deleteMutation.mutate(id)
  }

  const handleTestClick = (id: number) => {
    setTestingProviderId(id)
    setIsTestDialogOpen(true)
  }

  const handleEditClick = (provider: EmailProvider) => {
    setEditingProvider(provider)
    setIsEditDialogOpen(true)
  }

  const hasProviders = providers && providers.length > 0

  return (
    <div className="space-y-4">
      <div className="flex justify-between items-center">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">Email Providers</h2>
          <p className="text-muted-foreground">
            Configure cloud email providers like AWS SES or Scaleway to send
            emails.
          </p>
        </div>

        {hasProviders && (
          <CreateActionButton
            to="/email/providers/new"
            label="Add Provider"
            disabled={isEditDialogOpen || isTestDialogOpen}
          />
        )}
      </div>

      {isLoading ? (
        <LoadingSkeleton />
      ) : !hasProviders ? (
        <EmptyState
          icon={Mail}
          title="No email providers configured"
          description="Add your first email provider to start sending transactional emails from your applications."
          action={
            <CreateActionButton
              to="/email/providers/new"
              label="Add Provider"
              disabled={isEditDialogOpen || isTestDialogOpen}
            />
          }
        />
      ) : (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {providers.map((provider) => (
            <ProviderCard
              key={provider.id}
              provider={provider}
              onDelete={handleDelete}
              onTestClick={handleTestClick}
              onEditClick={handleEditClick}
            />
          ))}
        </div>
      )}

      <TestEmailDialog
        open={isTestDialogOpen}
        onOpenChange={setIsTestDialogOpen}
        providerId={testingProviderId}
        onSuccess={() => {
          queryClient.invalidateQueries({ queryKey: ['email-providers'] })
        }}
      />

      <EditProviderDialog
        open={isEditDialogOpen}
        onOpenChange={setIsEditDialogOpen}
        provider={editingProvider}
        onSuccess={() => {
          queryClient.invalidateQueries({ queryKey: ['email-providers'] })
        }}
      />
    </div>
  )
}
