'use client'

import {
  createEmailProvider as createProvider2,
  deleteEmailProvider as deleteProvider2,
  listEmailProviders as listProviders2,
  testProvider,
  updateEmailProvider as updateProvider2,
  type CreateEmailProviderRequest,
  type EmailProviderResponse,
  type TestEmailRequest as SdkTestEmailRequest,
  type TestEmailResponse as SdkTestEmailResponse,
  type UpdateEmailProviderRequest,
} from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
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
import { EllipsisVertical, Loader2, Mail, Plus, Send, Server } from 'lucide-react'
import { AWSIcon } from '@/components/icons/AWSIcon'
import { ScalewayIcon } from '@/components/icons/ScalewayIcon'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

// Types for email providers — alias over SDK response
type EmailProvider = EmailProviderResponse
type TestEmailResponse = SdkTestEmailResponse
type TestEmailRequest = SdkTestEmailRequest

// Form schema
const createProviderSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  provider_type: z.enum(['ses', 'scaleway', 'smtp']),
  region: z.string().min(1, 'Region is required'),
  // SES credentials
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
}).superRefine((data, ctx) => {
  if (data.provider_type === 'ses') {
    if (!data.access_key_id || !data.secret_access_key) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message: 'Access key ID and secret access key are required for AWS SES',
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
    // password is required when username is set
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

function problemMessage(error: unknown, fallback: string): string {
  if (error && typeof error === 'object' && 'detail' in error) {
    const detail = (error as { detail?: unknown }).detail
    if (typeof detail === 'string' && detail.length > 0) {
      return detail
    }
  }
  return fallback
}

async function listEmailProviders(): Promise<EmailProvider[]> {
  const response = await listProviders2()
  if (response.error) {
    throw new Error(problemMessage(response.error, 'Failed to fetch email providers'))
  }
  return response.data ?? []
}

async function createEmailProvider(data: CreateProviderFormData): Promise<EmailProvider> {
  const body: CreateEmailProviderRequest = {
    name: data.name,
    provider_type: data.provider_type,
    region: data.region,
  }

  if (data.provider_type === 'ses' && data.access_key_id && data.secret_access_key) {
    body.ses_credentials = {
      access_key_id: data.access_key_id,
      secret_access_key: data.secret_access_key,
    }
  } else if (data.provider_type === 'scaleway' && data.api_key && data.project_id) {
    body.scaleway_credentials = {
      api_key: data.api_key,
      project_id: data.project_id,
    }
  } else if (data.provider_type === 'smtp' && data.smtp_host && data.smtp_port) {
    body.smtp_credentials = {
      host: data.smtp_host,
      port: data.smtp_port,
      username: data.smtp_username || undefined,
      password: data.smtp_password || undefined,
      encryption: data.smtp_encryption ?? 'starttls',
      accept_invalid_certs: data.smtp_accept_invalid_certs ?? false,
    }
  }

  const response = await createProvider2({ body })
  if (response.error || !response.data) {
    throw new Error(problemMessage(response.error, 'Failed to create email provider'))
  }
  return response.data
}

async function deleteEmailProvider(id: number): Promise<void> {
  const response = await deleteProvider2({ path: { id } })
  if (response.error) {
    throw new Error(problemMessage(response.error, 'Failed to delete email provider'))
  }
}

async function updateEmailProviderApi(
  id: number,
  body: UpdateEmailProviderRequest,
): Promise<EmailProvider> {
  const response = await updateProvider2({ path: { id }, body })
  if (response.error || !response.data) {
    throw new Error(problemMessage(response.error, 'Failed to update email provider'))
  }
  return response.data
}

/**
 * The backend returns credentials as a freeform JSON value masked for display
 * (e.g. `{"host":"...", "port":587, "encryption":"starttls", "username":"AKIA...XYZ"}`).
 * This pulls out only the non-secret fields we need to prefill the edit form.
 */
function readMaskedCreds(credentials: unknown): {
  host?: string
  port?: number
  encryption?: 'starttls' | 'tls' | 'none'
  accept_invalid_certs?: boolean
} {
  if (!credentials || typeof credentials !== 'object') return {}
  const c = credentials as Record<string, unknown>
  const enc = typeof c.encryption === 'string' ? c.encryption : undefined
  return {
    host: typeof c.host === 'string' ? c.host : undefined,
    port: typeof c.port === 'number' ? c.port : undefined,
    encryption:
      enc === 'starttls' || enc === 'tls' || enc === 'none' ? enc : undefined,
    accept_invalid_certs:
      typeof c.accept_invalid_certs === 'boolean'
        ? c.accept_invalid_certs
        : undefined,
  }
}

async function testEmailProvider(id: number, request: TestEmailRequest): Promise<TestEmailResponse> {
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

// AWS regions for SES
const awsRegions = [
  { value: 'us-east-1', label: 'US East (N. Virginia)' },
  { value: 'us-east-2', label: 'US East (Ohio)' },
  { value: 'us-west-1', label: 'US West (N. California)' },
  { value: 'us-west-2', label: 'US West (Oregon)' },
  { value: 'eu-west-1', label: 'Europe (Ireland)' },
  { value: 'eu-west-2', label: 'Europe (London)' },
  { value: 'eu-west-3', label: 'Europe (Paris)' },
  { value: 'eu-central-1', label: 'Europe (Frankfurt)' },
  { value: 'ap-southeast-1', label: 'Asia Pacific (Singapore)' },
  { value: 'ap-southeast-2', label: 'Asia Pacific (Sydney)' },
  { value: 'ap-northeast-1', label: 'Asia Pacific (Tokyo)' },
  { value: 'ap-south-1', label: 'Asia Pacific (Mumbai)' },
  { value: 'sa-east-1', label: 'South America (São Paulo)' },
]

// Scaleway regions
const scalewayRegions = [
  { value: 'fr-par', label: 'Paris, France' },
  { value: 'nl-ams', label: 'Amsterdam, Netherlands' },
  { value: 'pl-waw', label: 'Warsaw, Poland' },
]

function ProviderIcon({ type }: { type: 'ses' | 'scaleway' | 'smtp' }) {
  if (type === 'ses') {
    return <AWSIcon className="h-5 w-5 text-[#FF9900]" />
  }
  if (type === 'scaleway') {
    return <ScalewayIcon className="h-5 w-5 text-[#4F0599]" />
  }
  return <Server className="h-5 w-5 text-slate-600 dark:text-slate-300" />
}

function providerTypeLabel(type: 'ses' | 'scaleway' | 'smtp'): string {
  switch (type) {
    case 'ses':
      return 'AWS SES'
    case 'scaleway':
      return 'Scaleway'
    case 'smtp':
      return 'SMTP'
  }
}

function TestEmailDialog({
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
    mutationFn: (data: TestEmailFormData) => testEmailProvider(providerId!, data),
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
            Enter the sender details to send a test email. The &quot;From&quot; address must be
            verified with your email provider.
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
                    <Input
                      placeholder="noreply@yourdomain.com"
                      {...field}
                    />
                  </FormControl>
                  <FormDescription>
                    The email address to send from. Must be verified with AWS SES or your domain must be verified with Scaleway.
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
                    <Input
                      placeholder="My Application"
                      {...field}
                    />
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

  const handleDelete = () => {
    setIsDeleting(true)
    try {
      onDelete(provider.id)
    } finally {
      setIsDeleting(false)
    }
  }

  return (
    <Card>
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
        <div className="flex items-center gap-2">
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
              <DropdownMenuItem
                onClick={() => onTestClick(provider.id)}
              >
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
              {formatDistanceToNow(new Date(provider.created_at), { addSuffix: true })}
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
        message: 'Provide both access key ID and secret access key, or leave both blank',
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

function EditProviderDialog({
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
            the stored credentials unchanged. The provider type cannot be changed.
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
                      <Select onValueChange={field.onChange} value={field.value}>
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
                          To rotate credentials, enter both the new access key ID
                          and the new secret. Leave both blank to keep the existing
                          credentials.
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
                          To rotate credentials, enter both the new API key and the
                          new project ID. Leave both blank to keep the existing values.
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
                            <SelectItem value="starttls">STARTTLS (port 587, default)</SelectItem>
                            <SelectItem value="tls">Implicit TLS / SMTPS (port 465)</SelectItem>
                            <SelectItem value="none">No encryption (local testing only)</SelectItem>
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
                          Required when setting a new password. Leave blank to keep
                          the stored value.
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
  const [isDialogOpen, setIsDialogOpen] = useState(false)
  const [isTestDialogOpen, setIsTestDialogOpen] = useState(false)
  const [testingProviderId, setTestingProviderId] = useState<number | null>(null)
  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)
  const [editingProvider, setEditingProvider] = useState<EmailProvider | null>(null)
  const queryClient = useQueryClient()

  const { data: providers, isLoading } = useQuery({
    queryKey: ['email-providers'],
    queryFn: listEmailProviders,
  })

  const createMutation = useMutation({
    mutationFn: createEmailProvider,
    onSuccess: () => {
      toast.success('Email provider created successfully')
      queryClient.invalidateQueries({ queryKey: ['email-providers'] })
      setIsDialogOpen(false)
      form.reset()
    },
    onError: (error: Error) => {
      toast.error('Failed to create provider', {
        description: error.message,
      })
    },
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

  const form = useForm<CreateProviderFormData>({
    resolver: zodResolver(createProviderSchema),
    defaultValues: {
      name: '',
      provider_type: 'ses',
      region: 'us-east-1',
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

  const providerType = form.watch('provider_type')
  const regions =
    providerType === 'ses'
      ? awsRegions
      : providerType === 'scaleway'
        ? scalewayRegions
        : []

  const onSubmit = (data: CreateProviderFormData) => {
    createMutation.mutate(data)
  }

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
            Configure cloud email providers like AWS SES or Scaleway to send emails.
          </p>
        </div>

        {hasProviders && (
          <Button onClick={() => setIsDialogOpen(true)}>
            <Plus className="h-4 w-4 mr-2" />
            Add Provider
          </Button>
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
            <Button onClick={() => setIsDialogOpen(true)}>
              <Plus className="h-4 w-4 mr-2" />
              Add Provider
            </Button>
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

      <Dialog open={isDialogOpen} onOpenChange={setIsDialogOpen}>
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>Add Email Provider</DialogTitle>
            <DialogDescription>
              Configure a cloud email provider to send transactional emails.
            </DialogDescription>
          </DialogHeader>

          <Form {...form}>
            <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
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

              <FormField
                control={form.control}
                name="provider_type"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Provider Type</FormLabel>
                    <Select
                      onValueChange={(value) => {
                        field.onChange(value)
                        // Reset region to a sensible default for the new provider
                        if (value === 'ses') {
                          form.setValue('region', 'us-east-1')
                        } else if (value === 'scaleway') {
                          form.setValue('region', 'fr-par')
                        } else if (value === 'smtp') {
                          // SMTP doesn't use cloud regions; reuse the field as a free-form label
                          form.setValue('region', 'custom')
                        }
                      }}
                      value={field.value}
                    >
                      <FormControl>
                        <SelectTrigger>
                          <SelectValue placeholder="Select a provider" />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value="ses">
                          <div className="flex items-center gap-2">
                            <AWSIcon className="h-4 w-4 text-[#FF9900]" />
                            AWS SES
                          </div>
                        </SelectItem>
                        <SelectItem value="scaleway">
                          <div className="flex items-center gap-2">
                            <ScalewayIcon className="h-4 w-4 text-[#4F0599]" />
                            Scaleway
                          </div>
                        </SelectItem>
                        <SelectItem value="smtp">
                          <div className="flex items-center gap-2">
                            <Server className="h-4 w-4 text-slate-600 dark:text-slate-300" />
                            SMTP (import existing domain)
                          </div>
                        </SelectItem>
                      </SelectContent>
                    </Select>
                    <FormDescription>
                      Choose <strong>SMTP</strong> when you already have SMTP credentials (for example,
                      AWS SES SMTP, Sendgrid, Mailgun) and your sending domain is verified at the
                      upstream provider — Temps will use those credentials directly without managing DNS.
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
                      <Select onValueChange={field.onChange} value={field.value}>
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
                    name="access_key_id"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Access Key ID</FormLabel>
                        <FormControl>
                          <Input placeholder="AKIAIOSFODNN7EXAMPLE" {...field} />
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
                            {...field}
                          />
                        </FormControl>
                        <FormDescription>
                          Your Scaleway secret key with Transactional Email permissions.
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
                  <div className="rounded-md border border-amber-200 bg-amber-50 dark:border-amber-900/40 dark:bg-amber-950/30 p-3 text-sm text-amber-900 dark:text-amber-100">
                    Domains added under an SMTP provider are treated as already verified —
                    Temps cannot manage DKIM/SPF/MX records via SMTP. Make sure DNS is configured
                    at your upstream provider (e.g. the AWS SES console) before sending.
                  </div>

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
                          <FormDescription>
                            For AWS SES: <code className="font-mono text-xs">email-smtp.&lt;region&gt;.amazonaws.com</code>.
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
                          onValueChange={(value) => {
                            field.onChange(value)
                            // Suggest the conventional port when switching modes,
                            // unless the user has already customised it.
                            const port = form.getValues('smtp_port')
                            if (value === 'starttls' && (port === 465 || port === 25)) {
                              form.setValue('smtp_port', 587)
                            } else if (value === 'tls' && (port === 587 || port === 25)) {
                              form.setValue('smtp_port', 465)
                            } else if (value === 'none' && (port === 587 || port === 465)) {
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
                            <SelectItem value="starttls">STARTTLS (port 587, default)</SelectItem>
                            <SelectItem value="tls">Implicit TLS / SMTPS (port 465)</SelectItem>
                            <SelectItem value="none">No encryption (local testing only)</SelectItem>
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
                          For AWS SES SMTP, this is the SMTP user generated in the SES console
                          (it is <em>not</em> your IAM access key).
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

              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => setIsDialogOpen(false)}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={createMutation.isPending}>
                  {createMutation.isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Add Provider
                </Button>
              </DialogFooter>
            </form>
          </Form>
        </DialogContent>
      </Dialog>

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
