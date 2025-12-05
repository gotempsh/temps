'use client'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
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
import { EmailProviderLogo, type EmailProviderType } from '@/components/ui/email-provider-logo'
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import {
  AlertCircle,
  CheckCircle2,
  Clock,
  EllipsisVertical,
  Globe,
  HelpCircle,
  Loader2,
  Plus,
  RefreshCw,
} from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

// Types
type DnsRecordStatus = 'unknown' | 'verified' | 'pending' | 'failed'

interface DnsRecord {
  record_type: string
  name: string
  value: string
  priority?: number
  status?: DnsRecordStatus
}

interface EmailDomain {
  id: number
  provider_id: number
  domain: string
  status: string
  last_verified_at: string | null
  verification_error: string | null
  created_at: string
  updated_at: string
}

interface EmailDomainWithDns {
  domain: EmailDomain
  dns_records: DnsRecord[]
}

interface EmailProvider {
  id: number
  name: string
  provider_type: 'ses' | 'scaleway'
  region: string
}

// Form schema
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

type CreateDomainFormData = z.infer<typeof createDomainSchema>

// API functions
async function listEmailDomains(): Promise<EmailDomain[]> {
  const response = await fetch('/api/email-domains')
  if (!response.ok) {
    throw new Error('Failed to fetch email domains')
  }
  return response.json()
}

async function getEmailDomain(id: number): Promise<EmailDomainWithDns> {
  const response = await fetch(`/api/email-domains/${id}`)
  if (!response.ok) {
    throw new Error('Failed to fetch email domain')
  }
  return response.json()
}

async function createEmailDomain(
  data: CreateDomainFormData
): Promise<EmailDomainWithDns> {
  const response = await fetch('/api/email-domains', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(data),
  })

  if (!response.ok) {
    const error = await response.json()
    throw new Error(error.detail || 'Failed to create email domain')
  }

  return response.json()
}

async function verifyEmailDomain(id: number): Promise<EmailDomainWithDns> {
  const response = await fetch(`/api/email-domains/${id}/verify`, {
    method: 'POST',
  })

  if (!response.ok) {
    const error = await response.json()
    throw new Error(error.detail || 'Failed to verify email domain')
  }

  return response.json()
}

async function deleteEmailDomain(id: number): Promise<void> {
  const response = await fetch(`/api/email-domains/${id}`, {
    method: 'DELETE',
  })

  if (!response.ok) {
    const error = await response.json()
    throw new Error(error.detail || 'Failed to delete email domain')
  }
}

async function listEmailProviders(): Promise<EmailProvider[]> {
  const response = await fetch('/api/email-providers')
  if (!response.ok) {
    throw new Error('Failed to fetch email providers')
  }
  return response.json()
}

function StatusBadge({ status }: { status: string }) {
  switch (status) {
    case 'verified':
      return (
        <Badge variant="default" className="gap-1 bg-green-600 hover:bg-green-600 text-white">
          <CheckCircle2 className="h-3 w-3" />
          Verified
        </Badge>
      )
    case 'pending':
      return (
        <Badge variant="secondary" className="gap-1">
          <Clock className="h-3 w-3" />
          Pending
        </Badge>
      )
    case 'failed':
      return (
        <Badge variant="destructive" className="gap-1">
          <AlertCircle className="h-3 w-3" />
          Failed
        </Badge>
      )
    default:
      return <Badge variant="outline">{status}</Badge>
  }
}

function DnsRecordStatusBadge({ status }: { status?: DnsRecordStatus }) {
  switch (status) {
    case 'verified':
      return (
        <div className="flex items-center gap-1.5 text-green-600 dark:text-green-500">
          <CheckCircle2 className="h-4 w-4" />
          <span className="text-xs font-medium">Verified</span>
        </div>
      )
    case 'pending':
      return (
        <div className="flex items-center gap-1.5 text-yellow-600 dark:text-yellow-500">
          <Clock className="h-4 w-4" />
          <span className="text-xs font-medium">Pending</span>
        </div>
      )
    case 'failed':
      return (
        <div className="flex items-center gap-1.5 text-destructive">
          <AlertCircle className="h-4 w-4" />
          <span className="text-xs font-medium">Failed</span>
        </div>
      )
    case 'unknown':
    default:
      return (
        <div className="flex items-center gap-1.5 text-muted-foreground">
          <HelpCircle className="h-4 w-4" />
          <span className="text-xs font-medium">Unknown</span>
        </div>
      )
  }
}

function DnsVerificationSummary({ records }: { records: DnsRecord[] }) {
  const verifiedCount = records.filter(r => r.status === 'verified').length
  const pendingCount = records.filter(r => r.status === 'pending').length
  const failedCount = records.filter(r => r.status === 'failed').length
  const unknownCount = records.filter(r => !r.status || r.status === 'unknown').length
  const totalCount = records.length

  const allVerified = verifiedCount === totalCount && totalCount > 0

  if (allVerified) {
    return (
      <div className="flex items-center gap-2 p-3 rounded-lg bg-green-50 dark:bg-green-950/30 border border-green-200 dark:border-green-900">
        <CheckCircle2 className="h-5 w-5 text-green-600 dark:text-green-500" />
        <span className="text-sm font-medium text-green-700 dark:text-green-400">
          All {totalCount} DNS records verified successfully
        </span>
      </div>
    )
  }

  return (
    <div className="flex items-center gap-4 p-3 rounded-lg bg-muted/50 border">
      <span className="text-sm font-medium">DNS Status:</span>
      <div className="flex items-center gap-4 text-sm">
        {verifiedCount > 0 && (
          <div className="flex items-center gap-1 text-green-600 dark:text-green-500">
            <CheckCircle2 className="h-4 w-4" />
            <span>{verifiedCount} verified</span>
          </div>
        )}
        {pendingCount > 0 && (
          <div className="flex items-center gap-1 text-yellow-600 dark:text-yellow-500">
            <Clock className="h-4 w-4" />
            <span>{pendingCount} pending</span>
          </div>
        )}
        {failedCount > 0 && (
          <div className="flex items-center gap-1 text-destructive">
            <AlertCircle className="h-4 w-4" />
            <span>{failedCount} failed</span>
          </div>
        )}
        {unknownCount > 0 && (
          <div className="flex items-center gap-1 text-muted-foreground">
            <HelpCircle className="h-4 w-4" />
            <span>{unknownCount} unknown</span>
          </div>
        )}
      </div>
    </div>
  )
}

function DnsRecordsTable({ records }: { records: DnsRecord[] }) {
  if (!records || records.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">No DNS records available.</p>
    )
  }

  return (
    <div className="rounded-md border">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead className="w-[100px]">Type</TableHead>
            <TableHead>Name</TableHead>
            <TableHead>Value</TableHead>
            <TableHead className="w-[80px]">Priority</TableHead>
            <TableHead className="w-[100px]">Status</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {records.map((record, index) => (
            <TableRow
              key={index}
              className={
                record.status === 'verified'
                  ? 'bg-green-50/50 dark:bg-green-950/20'
                  : record.status === 'failed'
                    ? 'bg-red-50/50 dark:bg-red-950/20'
                    : ''
              }
            >
              <TableCell>
                <Badge variant="outline">{record.record_type}</Badge>
              </TableCell>
              <TableCell>
                <div className="flex items-center gap-2">
                  <span className="font-mono text-xs break-all">{record.name}</span>
                  <CopyButton
                    value={record.name}
                    className="h-6 w-6 p-0 hover:bg-accent hover:text-accent-foreground rounded-md flex-shrink-0"
                  />
                </div>
              </TableCell>
              <TableCell>
                <div className="flex items-center gap-2">
                  <span className="font-mono text-xs break-all">{record.value}</span>
                  <CopyButton
                    value={record.value}
                    className="h-6 w-6 p-0 hover:bg-accent hover:text-accent-foreground rounded-md flex-shrink-0"
                  />
                </div>
              </TableCell>
              <TableCell>{record.priority ?? '-'}</TableCell>
              <TableCell>
                <DnsRecordStatusBadge status={record.status} />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  )
}

function DomainCard({
  domain,
  onVerify,
  onDelete,
  onViewDetails,
}: {
  domain: EmailDomain
  onVerify: (id: number) => void
  onDelete: (id: number) => void
  onViewDetails: (id: number) => void
}) {
  return (
    <Card>
      <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
        <div className="flex items-center gap-3">
          <Globe className="h-5 w-5 text-muted-foreground" />
          <div>
            <CardTitle className="text-base font-medium leading-none">
              {domain.domain}
            </CardTitle>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <StatusBadge status={domain.status} />
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon" className="h-8 w-8">
                <EllipsisVertical className="h-4 w-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onClick={() => onViewDetails(domain.id)}>
                View DNS Records
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => onVerify(domain.id)}>
                <RefreshCw className="h-4 w-4 mr-2" />
                Verify DNS
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                className="text-destructive"
                onClick={() => onDelete(domain.id)}
              >
                Delete
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </CardHeader>
      <CardContent>
        <div className="space-y-2 text-sm">
          {domain.verification_error && (
            <div className="text-destructive text-xs">
              {domain.verification_error}
            </div>
          )}
          <div className="flex justify-between">
            <span className="text-muted-foreground">Last Verified</span>
            <span>
              {domain.last_verified_at
                ? formatDistanceToNow(new Date(domain.last_verified_at), {
                    addSuffix: true,
                  })
                : 'Never'}
            </span>
          </div>
          <div className="flex justify-between">
            <span className="text-muted-foreground">Created</span>
            <span>
              {formatDistanceToNow(new Date(domain.created_at), {
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

export function EmailDomainsManagement() {
  const [isCreateDialogOpen, setIsCreateDialogOpen] = useState(false)
  const [selectedDomainId, setSelectedDomainId] = useState<number | null>(null)
  const queryClient = useQueryClient()

  const { data: domains, isLoading: isLoadingDomains } = useQuery({
    queryKey: ['email-domains'],
    queryFn: listEmailDomains,
  })

  const { data: providers, isLoading: isLoadingProviders } = useQuery({
    queryKey: ['email-providers'],
    queryFn: listEmailProviders,
  })

  const { data: selectedDomainDetails } = useQuery({
    queryKey: ['email-domain', selectedDomainId],
    queryFn: () =>
      selectedDomainId ? getEmailDomain(selectedDomainId) : null,
    enabled: !!selectedDomainId,
  })

  const createMutation = useMutation({
    mutationFn: createEmailDomain,
    onSuccess: () => {
      toast.success('Domain added successfully', {
        description:
          'Please configure the DNS records shown below to verify your domain.',
      })
      queryClient.invalidateQueries({ queryKey: ['email-domains'] })
      setIsCreateDialogOpen(false)
      form.reset()
    },
    onError: (error: Error) => {
      toast.error('Failed to add domain', {
        description: error.message,
      })
    },
  })

  const verifyMutation = useMutation({
    mutationFn: verifyEmailDomain,
    onSuccess: (data) => {
      const verifiedCount = data.dns_records.filter(r => r.status === 'verified').length
      const totalCount = data.dns_records.length
      const pendingCount = data.dns_records.filter(r => r.status === 'pending').length
      const failedCount = data.dns_records.filter(r => r.status === 'failed').length

      if (data.domain.status === 'verified') {
        toast.success('Domain verified successfully', {
          description: `All ${totalCount} DNS records are properly configured.`,
        })
      } else if (failedCount > 0) {
        toast.error('Some DNS records failed verification', {
          description: `${failedCount} of ${totalCount} records failed. Please check your DNS configuration.`,
        })
      } else if (pendingCount > 0) {
        toast.warning('DNS verification in progress', {
          description: `${verifiedCount} of ${totalCount} records verified. ${pendingCount} pending - DNS propagation can take up to 48 hours.`,
        })
      } else {
        toast.info('Verification status updated', {
          description: `${verifiedCount} of ${totalCount} records verified.`,
        })
      }

      // Update the domain details cache directly with the new data
      queryClient.setQueryData(['email-domain', data.domain.id], data)

      // Update the domains list cache to reflect the new status
      queryClient.setQueryData(['email-domains'], (oldDomains: EmailDomain[] | undefined) => {
        if (!oldDomains) return oldDomains
        return oldDomains.map((d) =>
          d.id === data.domain.id ? data.domain : d
        )
      })

      // Also refetch to ensure we have the latest data
      queryClient.refetchQueries({ queryKey: ['email-domains'] })
      queryClient.refetchQueries({ queryKey: ['email-domain', data.domain.id] })
    },
    onError: (error: Error) => {
      toast.error('Failed to verify domain', {
        description: error.message,
      })
    },
  })

  const deleteMutation = useMutation({
    mutationFn: deleteEmailDomain,
    onSuccess: () => {
      toast.success('Domain deleted successfully')
      queryClient.invalidateQueries({ queryKey: ['email-domains'] })
    },
    onError: (error: Error) => {
      toast.error('Failed to delete domain', {
        description: error.message,
      })
    },
  })

  const form = useForm<CreateDomainFormData>({
    resolver: zodResolver(createDomainSchema),
    defaultValues: {
      domain: '',
    },
  })

  const onSubmit = (data: CreateDomainFormData) => {
    createMutation.mutate(data)
  }

  const handleVerify = (id: number) => {
    verifyMutation.mutate(id)
  }

  const handleDelete = (id: number) => {
    deleteMutation.mutate(id)
  }

  const handleViewDetails = (id: number) => {
    setSelectedDomainId(id)
  }

  const hasDomains = domains && domains.length > 0
  const hasProviders = providers && providers.length > 0

  return (
    <div className="space-y-4">
      <div className="flex justify-between items-center">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">Email Domains</h2>
          <p className="text-muted-foreground">
            Configure domains for sending emails. DNS verification is required.
          </p>
        </div>

        {hasDomains && hasProviders && (
          <Button onClick={() => setIsCreateDialogOpen(true)}>
            <Plus className="h-4 w-4 mr-2" />
            Add Domain
          </Button>
        )}
      </div>

      {isLoadingDomains || isLoadingProviders ? (
        <LoadingSkeleton />
      ) : !hasProviders ? (
        <EmptyState
          icon={Globe}
          title="No email providers configured"
          description="You need to configure an email provider before adding domains. Go to the Providers tab to add one."
        />
      ) : !hasDomains ? (
        <EmptyState
          icon={Globe}
          title="No email domains configured"
          description="Add a domain to start sending emails. You'll need to configure DNS records for verification."
          action={
            <Button onClick={() => setIsCreateDialogOpen(true)}>
              <Plus className="h-4 w-4 mr-2" />
              Add Domain
            </Button>
          }
        />
      ) : (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {domains.map((domain) => (
            <DomainCard
              key={domain.id}
              domain={domain}
              onVerify={handleVerify}
              onDelete={handleDelete}
              onViewDetails={handleViewDetails}
            />
          ))}
        </div>
      )}

      {/* Create Domain Dialog */}
      <Dialog open={isCreateDialogOpen} onOpenChange={setIsCreateDialogOpen}>
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>Add Email Domain</DialogTitle>
            <DialogDescription>
              Add a domain for sending emails. You'll need to configure DNS
              records after adding.
            </DialogDescription>
          </DialogHeader>

          <Form {...form}>
            <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-6">
              <FormField
                control={form.control}
                name="provider_id"
                render={({ field }) => {
                  const selectedProvider = providers?.find(p => p.id === field.value)
                  return (
                    <FormItem>
                      <FormLabel>Provider</FormLabel>
                      <Select
                        onValueChange={(value) => field.onChange(parseInt(value))}
                        value={field.value?.toString()}
                      >
                        <FormControl>
                          <SelectTrigger>
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
                        </FormControl>
                        <SelectContent>
                          {providers?.map((provider) => (
                            <SelectItem
                              key={provider.id}
                              value={provider.id.toString()}
                            >
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
                      <FormDescription>
                        The email provider to use for this domain.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )
                }}
              />

              <FormField
                control={form.control}
                name="domain"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Domain</FormLabel>
                    <FormControl>
                      <Input placeholder="send.example.com" {...field} />
                    </FormControl>
                    <FormDescription>
                      Use a subdomain (e.g., send.example.com) to isolate your
                      email sending reputation and protect your primary domain.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => setIsCreateDialogOpen(false)}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={createMutation.isPending}>
                  {createMutation.isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Add Domain
                </Button>
              </DialogFooter>
            </form>
          </Form>
        </DialogContent>
      </Dialog>

      {/* DNS Records Dialog */}
      <Dialog
        open={!!selectedDomainId}
        onOpenChange={(open) => !open && setSelectedDomainId(null)}
      >
        <DialogContent className="max-w-4xl max-h-[90vh] overflow-y-auto">
          <DialogHeader>
            <DialogTitle>DNS Records</DialogTitle>
            <DialogDescription>
              Add these DNS records to your domain's DNS settings to verify
              ownership and enable email sending.
            </DialogDescription>
          </DialogHeader>

          {selectedDomainDetails ? (
            <div className="space-y-4">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <Globe className="h-5 w-5 text-muted-foreground" />
                  <span className="font-medium">
                    {selectedDomainDetails.domain.domain}
                  </span>
                </div>
                <StatusBadge status={selectedDomainDetails.domain.status} />
              </div>

              <DnsVerificationSummary records={selectedDomainDetails.dns_records} />

              <DnsRecordsTable records={selectedDomainDetails.dns_records} />

              {selectedDomainDetails.domain.status !== 'verified' && (
                <div className="bg-muted/50 p-4 rounded-lg">
                  <h4 className="font-medium mb-2">How to configure DNS:</h4>
                  <ol className="list-decimal list-inside space-y-1 text-sm text-muted-foreground">
                    <li>
                      Log in to your domain registrar or DNS provider (e.g.,
                      Cloudflare, Route53, GoDaddy)
                    </li>
                    <li>Navigate to the DNS management section</li>
                    <li>Add each record shown above with the exact values</li>
                    <li>
                      Wait for DNS propagation (can take up to 48 hours, usually
                      much faster)
                    </li>
                    <li>
                      Click "Verify DNS" to check if the records are properly
                      configured
                    </li>
                  </ol>
                </div>
              )}
            </div>
          ) : (
            <div className="flex items-center justify-center py-8">
              <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
            </div>
          )}

          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setSelectedDomainId(null)}
            >
              Close
            </Button>
            {selectedDomainId && (
              <Button
                onClick={() => handleVerify(selectedDomainId)}
                disabled={verifyMutation.isPending}
              >
                {verifyMutation.isPending && (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                )}
                <RefreshCw className="mr-2 h-4 w-4" />
                Verify DNS
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
