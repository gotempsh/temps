// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  addManagedDomain,
  applyHostnameMode,
  deleteDnsProvider as deleteProvider,
  getDnsProvider as getProvider,
  listManagedDomains,
  listProviderZones,
  previewHostnameMode,
  removeManagedDomain,
  testProviderConnection,
  updateManagedDomain,
  updateProvider,
  verifyManagedDomain,
  type HostnamePreviewResponse,
  type ManagedDomainResponse,
  type UpdateDnsProviderRequest,
} from '@/api/client'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
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
import { SearchableSelect } from '@/components/ui/searchable-select'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import {
  deliveryError,
  requireDeliveryData,
} from '@/components/domains/delivery-errors'
import { HostnameConflictList } from '@/components/domains/HostnameConflictList'
import {
  conflictDecisionsRequest,
  conflictKey,
  plannedDnsChanges,
  unresolvedConflicts,
  type ConflictDecisions,
} from '@/components/domains/hostname-conflicts'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  Button,
  Callout,
  Detail,
  PageState,
  Status,
  fmtDateTime,
  fmtRelativeTime,
  type DetailFact,
  type StatusTone,
} from '@temps-sdk/ds'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  CheckCircle2,
  Cloud,
  Edit,
  Globe,
  Loader2,
  Plus,
  RefreshCw,
  TestTube2,
  Trash2,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

// Helper function to get provider icon
function getProviderIcon(providerType: string, className = 'h-5 w-5') {
  switch (providerType.toLowerCase()) {
    case 'bunny':
      return (
        <img
          src="/providers/bunny-official.svg"
          alt="bunny.net"
          className={className}
        />
      )
    case 'cloudflare':
      return <Cloud className={`${className} text-orange-500`} />
    default:
      return <Globe className={className} />
  }
}

// Helper function to format provider type for display
function formatProviderType(type: string): string {
  switch (type.toLowerCase()) {
    case 'bunny':
      return 'bunny.net DNS'
    case 'cloudflare':
      return 'Cloudflare'
    case 'namecheap':
      return 'Namecheap'
    default:
      return type.charAt(0).toUpperCase() + type.slice(1)
  }
}

// Edit form schema
const editFormSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  description: z.string().optional(),
  is_active: z.boolean(),
})

type EditFormData = z.infer<typeof editFormSchema>

// Add domain form schema
const addDomainFormSchema = z.object({
  domain: z
    .string()
    .min(1, 'Choose a DNS zone')
    .regex(
      /^([a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$/,
      'Invalid DNS zone name'
    ),
  auto_manage: z.boolean(),
  proxied_by_default: z.boolean(),
})

type AddDomainFormData = z.infer<typeof addDomainFormSchema>

// The record recipe's single verdict, derived from is_active / last_error
// rather than inventing a new severity ordering: an inactive provider is an
// intentional off-state (idle), an active one with a recent error is a
// warning, and a healthy active provider is ok.
function providerVerdict(provider: {
  is_active: boolean
  last_error?: string | null
}): { tone: StatusTone; label: string } {
  if (!provider.is_active) return { tone: 'idle', label: 'Inactive' }
  if (provider.last_error) return { tone: 'warn', label: 'Active — error' }
  return { tone: 'ok', label: 'Active' }
}

function providerFacts(
  provider: { provider_type: string; is_active: boolean; created_at: string },
  managedDomainsCount: number | undefined,
  zonesCount: number | undefined
): DetailFact[] {
  return [
    {
      label: 'Type',
      value: (
        <span className="inline-flex items-center gap-1.5">
          {getProviderIcon(provider.provider_type, 'h-3.5 w-3.5')}
          {formatProviderType(provider.provider_type)}
        </span>
      ),
    },
    {
      label: 'Managed zones',
      value: managedDomainsCount !== undefined ? managedDomainsCount : '—',
    },
    {
      label: 'Available zones',
      value: !provider.is_active
        ? 'Inactive'
        : zonesCount !== undefined
          ? zonesCount
          : '—',
    },
    {
      label: 'Added',
      value: (
        <span title={fmtDateTime(provider.created_at)}>
          {fmtRelativeTime(provider.created_at)}
        </span>
      ),
    },
  ]
}

function DnsProviderDetailSkeleton({
  backAction,
}: {
  backAction: React.ReactNode
}) {
  return (
    <Detail
      title={<Skeleton className="h-7 w-56" />}
      actions={backAction}
      facts={[0, 1, 2, 3].map(() => ({
        label: <Skeleton className="h-3 w-16" />,
        value: <Skeleton className="h-4 w-24" />,
      }))}
      main={
        <Card>
          <CardHeader>
            <Skeleton className="h-5 w-40" />
            <Skeleton className="mt-2 h-4 w-64" />
          </CardHeader>
          <CardContent className="space-y-4">
            <Skeleton className="h-14 w-full rounded-lg" />
            <Skeleton className="h-14 w-full rounded-lg" />
          </CardContent>
        </Card>
      }
      aside={<Skeleton className="h-48 w-full rounded-lg" />}
    />
  )
}

export default function DnsProviderDetail() {
  const { id } = useParams<{ id: string }>()
  const providerId = parseInt(id || '0', 10)
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)
  const [isDeleteDialogOpen, setIsDeleteDialogOpen] = useState(false)
  const [isAddDomainDialogOpen, setIsAddDomainDialogOpen] = useState(false)
  const [domainToRemove, setDomainToRemove] =
    useState<ManagedDomainResponse | null>(null)

  // Queries
  const {
    data: provider,
    isLoading,
    error,
    refetch,
  } = useQuery({
    queryKey: ['dnsProvider', providerId],
    queryFn: async () => {
      const response = await getProvider({ path: { id: providerId } })
      return requireDeliveryData(response)
    },
    enabled: !!providerId,
  })

  const {
    data: managedDomains,
    isLoading: domainsLoading,
    error: domainsError,
    refetch: refetchDomains,
  } = useQuery({
    queryKey: ['dnsProviderDomains', providerId],
    queryFn: async () => {
      const response = await listManagedDomains({ path: { id: providerId } })
      return requireDeliveryData(response)
    },
    enabled: !!providerId,
  })

  const {
    data: zones,
    isPending: zonesPending,
    isError: zonesError,
    error: zonesQueryError,
    refetch: refetchZones,
  } = useQuery({
    queryKey: ['dnsProviderZones', providerId],
    queryFn: async () => {
      const response = await listProviderZones({ path: { id: providerId } })
      return requireDeliveryData(response)
    },
    enabled: !!providerId && !!provider?.is_active,
  })
  const selectableZones = (zones?.zones ?? []).filter(
    (zone) =>
      !managedDomains?.some(
        (managed) => managed.domain.toLowerCase() === zone.name.toLowerCase()
      )
  )

  // Mutations
  const updateProviderMut = useMutation({
    mutationFn: async (data: Partial<EditFormData>) => {
      const body: UpdateDnsProviderRequest = {
        name: data.name,
        description: data.description,
        is_active: data.is_active,
      }
      const response = await updateProvider({ path: { id: providerId }, body })
      return requireDeliveryData(response)
    },
    onSuccess: () => {
      toast.success('Provider updated successfully')
      queryClient.invalidateQueries({ queryKey: ['dnsProvider', providerId] })
      queryClient.invalidateQueries({ queryKey: ['dnsProviders'] })
      setIsEditDialogOpen(false)
    },
    onError: (err: Error) => {
      toast.error('Failed to update provider', {
        description: err.message,
      })
    },
  })

  const deleteProviderMut = useMutation({
    mutationFn: async () => {
      const response = await deleteProvider({ path: { id: providerId } })
      if (response.error) throw new Error(deliveryError(response.error))
    },
    onSuccess: () => {
      toast.success('Provider deleted successfully')
      queryClient.invalidateQueries({ queryKey: ['dnsProviders'] })
      navigate('/dns-providers')
    },
    onError: (err: Error) => {
      toast.error('Failed to delete provider', {
        description: err.message,
      })
    },
  })

  const testConnectionMut = useMutation({
    mutationFn: async () => {
      const response = await testProviderConnection({
        path: { id: providerId },
      })
      return requireDeliveryData(response)
    },
    onSuccess: (result) => {
      if (result?.success) {
        toast.success('Connection test successful', {
          description: result.message,
        })
      } else {
        toast.error('Connection test failed', {
          description: result?.message,
        })
      }
      refetch()
    },
    onError: (err: Error) => {
      toast.error('Connection test failed', {
        description: err.message,
      })
    },
  })

  const addDomainMut = useMutation({
    mutationFn: async (data: AddDomainFormData) => {
      const response = await addManagedDomain({
        path: { id: providerId },
        body: {
          domain: data.domain,
          auto_manage: data.auto_manage,
          proxied_by_default: data.proxied_by_default,
          generated_hostname_mode: data.proxied_by_default
            ? 'flat'
            : 'standard',
          sync_generated_records: data.proxied_by_default,
        },
      })
      return requireDeliveryData(response)
    },
    onSuccess: () => {
      toast.success('Zone added successfully')
      refetchDomains()
      setIsAddDomainDialogOpen(false)
      addDomainForm.reset()
    },
    onError: (err: Error) => {
      toast.error('Failed to add zone', {
        description: err.message,
      })
    },
  })

  const removeDomainMut = useMutation({
    mutationFn: async (domain: string) => {
      const response = await removeManagedDomain({
        path: { provider_id: providerId, domain },
      })
      if (response.error) throw new Error(deliveryError(response.error))
    },
    onSuccess: () => {
      toast.success('Zone removed successfully')
      refetchDomains()
      setDomainToRemove(null)
    },
    onError: (err: Error) => {
      toast.error('Failed to remove zone', {
        description: err.message,
      })
    },
  })

  const verifyDomainMut = useMutation({
    mutationFn: async (domain: string) => {
      const result = requireDeliveryData(
        await verifyManagedDomain({
          path: { provider_id: providerId, domain },
        })
      )
      return result
    },
    onSuccess: (result) => {
      if (result.verified) {
        toast.success('Zone access verified')
      } else {
        toast.error('Zone access could not be verified', {
          description:
            result.zone_access_error ??
            result.verification_error ??
            'Check this provider’s zone permissions and try again.',
        })
      }
      refetchDomains()
    },
    onError: (err: Error) => {
      toast.error('Failed to verify zone access', {
        description: deliveryError(err),
      })
    },
  })

  // Per-domain hostname mode: preview before an explicit, breaking apply.
  const [hostnamePreview, setHostnamePreview] = useState<{
    domain: string
    target: 'standard' | 'flat'
    syncDns: boolean
    result: HostnamePreviewResponse
  } | null>(null)
  // The user's adopt/skip choice per conflicting DNS record of the preview.
  // Reset with every preview: a decision only covers the record the user saw.
  const [conflictDecisions, setConflictDecisions] = useState<ConflictDecisions>(
    {}
  )

  const previewModeMut = useMutation({
    mutationFn: async (vars: {
      domain: string
      target: 'standard' | 'flat'
      syncDns: boolean
    }) =>
      requireDeliveryData(
        await previewHostnameMode({
          path: { provider_id: providerId, domain: vars.domain },
          query: { mode: vars.target, sync: vars.syncDns },
        })
      ),
    onSuccess: (result, vars) => {
      setConflictDecisions({})
      applyModeMut.reset()
      setHostnamePreview({ ...vars, result })
    },
    onError: (err: Error) => {
      toast.error('Failed to preview hostname change', {
        description: err.message,
      })
    },
  })

  const applyModeMut = useMutation({
    mutationFn: async (vars: {
      domain: string
      target: 'standard' | 'flat'
      syncDns: boolean
      decisions: ReturnType<typeof conflictDecisionsRequest>
    }) =>
      requireDeliveryData(
        await applyHostnameMode({
          path: { provider_id: providerId, domain: vars.domain },
          body: {
            mode: vars.target,
            sync_dns: vars.syncDns,
            ...vars.decisions,
          },
        })
      ),
    onSuccess: () => {
      toast.success('Hostname mode applied')
      closeHostnamePreview()
      refetchDomains()
    },
    onError: (err: Error) => {
      toast.error('Failed to apply hostname mode', {
        description: err.message,
      })
      // An apply that stopped part-way may still have saved the new mode.
      refetchDomains()
    },
  })

  const closeHostnamePreview = () => {
    setHostnamePreview(null)
    setConflictDecisions({})
    applyModeMut.reset()
  }
  const hostnameConflicts = hostnamePreview?.syncDns
    ? hostnamePreview.result.conflicts
    : []
  const undecidedConflicts = unresolvedConflicts(
    hostnameConflicts,
    conflictDecisions
  )
  const plannedChanges = plannedDnsChanges(
    hostnamePreview?.result.dns_changes ?? []
  )

  const syncToggleMut = useMutation({
    mutationFn: async (vars: { domain: string; enabled: boolean }) =>
      requireDeliveryData(
        await updateManagedDomain({
          path: { provider_id: providerId, domain: vars.domain },
          body: { sync_generated_records: vars.enabled },
        })
      ),
    onSuccess: () => {
      refetchDomains()
    },
    onError: (err: Error) => {
      toast.error('Failed to update DNS sync setting', {
        description: err.message,
      })
    },
  })

  const proxyToggleMut = useMutation({
    mutationFn: async (vars: { domain: string; enabled: boolean }) =>
      requireDeliveryData(
        await updateManagedDomain({
          path: { provider_id: providerId, domain: vars.domain },
          body: { proxied_by_default: vars.enabled },
        })
      ),
    onSuccess: (_data, vars) => {
      toast.success(
        vars.enabled ? 'Cloudflare proxy enabled' : 'Cloudflare proxy disabled'
      )
      refetchDomains()
    },
    onError: (err: Error) => {
      toast.error('Failed to update proxy setting', {
        description: err.message,
      })
    },
  })

  // Forms
  const editForm = useForm<EditFormData>({
    resolver: zodResolver(editFormSchema),
    defaultValues: {
      name: provider?.name || '',
      description: provider?.description || '',
      is_active: provider?.is_active ?? true,
    },
  })

  const addDomainForm = useForm<AddDomainFormData>({
    resolver: zodResolver(addDomainFormSchema),
    defaultValues: {
      domain: '',
      auto_manage: true,
      proxied_by_default: false,
    },
  })

  // Update form values when provider loads
  useEffect(() => {
    if (provider) {
      editForm.reset({
        name: provider.name,
        description: provider.description || '',
        is_active: provider.is_active,
      })
    }
  }, [provider, editForm])

  useEffect(() => {
    if (provider) {
      setBreadcrumbs([
        { label: 'DNS Providers', href: '/dns-providers' },
        { label: provider.name },
      ])
    }
  }, [provider, setBreadcrumbs])

  usePageTitle(provider?.name || 'DNS Provider')

  const backAction = (
    <Button variant="ghost" size="sm" asChild>
      <Link to="/dns-providers">
        <ArrowLeft className="mr-2 size-4" />
        Back to providers
      </Link>
    </Button>
  )

  if (isLoading) {
    return <DnsProviderDetailSkeleton backAction={backAction} />
  }

  if (error || !provider) {
    return (
      <PageState
        variant="failed"
        icon={AlertCircle}
        title="Couldn't load DNS provider"
        description="This provider may have been deleted, or you may not have permission to view it."
        action={<Button onClick={() => void refetch()}>Retry</Button>}
      />
    )
  }

  const providerSupportsZoneSelection = ['cloudflare', 'bunny'].includes(
    provider.provider_type.toLowerCase()
  )
  const verdict = providerVerdict(provider)

  return (
    <>
      <Detail
        title={provider.name}
        description={provider.description}
        verdict={<Status tone={verdict.tone} label={verdict.label} />}
        actions={
          <>
            {backAction}
            <Button
              variant="outline"
              size="sm"
              onClick={() => testConnectionMut.mutate()}
              busy={testConnectionMut.isPending}
              busyLabel="Testing…"
            >
              <TestTube2 className="mr-2 size-4" />
              Test Connection
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setIsEditDialogOpen(true)}
            >
              <Edit className="mr-2 size-4" />
              Edit
            </Button>
            <Button
              variant="destructive"
              size="sm"
              onClick={() => setIsDeleteDialogOpen(true)}
            >
              <Trash2 className="mr-2 size-4" />
              Delete
            </Button>
          </>
        }
        facts={providerFacts(
          provider,
          managedDomains?.length,
          zones?.zones.length
        )}
        main={
          <>
            {provider.last_error && (
              <Callout tone="error" title="Last error">
                <span className="break-all font-mono text-xs">
                  {provider.last_error}
                </span>
              </Callout>
            )}

            {/* Managed DNS zones */}
            <Card>
              <CardHeader className="flex flex-row items-center justify-between">
                <div>
                  <CardTitle>Managed zones</CardTitle>
                  <CardDescription>
                    DNS zones Temps can use to manage records for projects
                  </CardDescription>
                </div>
                <div className="flex items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => refetchDomains()}
                  >
                    <RefreshCw className="h-4 w-4" />
                  </Button>
                  <Button
                    size="sm"
                    onClick={() => setIsAddDomainDialogOpen(true)}
                  >
                    <Plus className="mr-2 h-4 w-4" />
                    Add zone
                  </Button>
                </div>
              </CardHeader>
              <CardContent>
                {domainsError ? (
                  <Alert variant="destructive">
                    <AlertDescription>
                      {deliveryError(domainsError)}{' '}
                      <Button variant="link" onClick={() => refetchDomains()}>
                        Retry
                      </Button>
                    </AlertDescription>
                  </Alert>
                ) : domainsLoading ? (
                  <Skeleton className="h-20 w-full" />
                ) : !managedDomains?.length ? (
                  <div className="text-center py-8 text-muted-foreground">
                    <Globe className="h-12 w-12 mx-auto mb-4 opacity-50" />
                    <p>No managed zones yet</p>
                    <p className="text-sm">
                      Choose Add zone, select an existing account zone, then
                      verify access. Configure a hostname from your project’s
                      Domains page.
                    </p>
                  </div>
                ) : (
                  <ul role="list" className="divide-y rounded-md border">
                    {managedDomains.map((domain) => (
                      <li
                        key={domain.id}
                        className="flex items-center justify-between gap-3 px-4 py-3"
                      >
                        <div className="min-w-0 space-y-1">
                          <div className="flex flex-wrap items-center gap-2">
                            <p className="truncate font-medium">
                              {domain.domain}
                            </p>
                            {domain.verified ? (
                              <Badge
                                variant="secondary"
                                className="flex items-center gap-1"
                              >
                                <CheckCircle2 className="h-3 w-3" />
                                Verified
                              </Badge>
                            ) : (
                              <Badge
                                variant="outline"
                                className="flex items-center gap-1"
                              >
                                <XCircle className="h-3 w-3" />
                                Not Verified
                              </Badge>
                            )}
                            {domain.auto_manage && (
                              <Badge variant="outline">Auto-managed</Badge>
                            )}
                            <Badge
                              variant={
                                domain.generated_hostname_mode === 'flat'
                                  ? 'default'
                                  : 'outline'
                              }
                            >
                              {domain.generated_hostname_mode === 'flat'
                                ? 'Flat hostnames'
                                : 'Standard hostnames'}
                            </Badge>
                            {domain.zone_access_ok === false && (
                              <Badge
                                variant="destructive"
                                className="flex items-center gap-1"
                              >
                                <XCircle className="h-3 w-3" />
                                Token lacks zone access
                              </Badge>
                            )}
                          </div>
                          {domain.zone_id && (
                            <p className="truncate text-sm text-muted-foreground">
                              Zone ID: {domain.zone_id}
                            </p>
                          )}
                          {domain.verification_error && (
                            <p className="truncate text-sm text-destructive">
                              {domain.verification_error}
                            </p>
                          )}
                          {domain.zone_access_error && (
                            <p className="truncate text-sm text-destructive">
                              {domain.zone_access_error}
                            </p>
                          )}
                          {provider?.flat_hostnames_supported && (
                            <div className="grid gap-3 pt-1 sm:grid-cols-2">
                              <div className="space-y-1">
                                <label className="flex items-center gap-2 text-sm">
                                  <Switch
                                    checked={
                                      domain.generated_hostname_mode === 'flat'
                                    }
                                    onCheckedChange={(checked) =>
                                      previewModeMut.mutate({
                                        domain: domain.domain,
                                        target: checked ? 'flat' : 'standard',
                                        syncDns: domain.sync_generated_records,
                                      })
                                    }
                                    disabled={previewModeMut.isPending}
                                  />
                                  Flat hostnames
                                </label>
                                <p className="pl-12 text-xs text-muted-foreground">
                                  Keeps generated addresses one level below this
                                  zone so Cloudflare Universal SSL can cover
                                  them. Changing this previews the affected
                                  routes before applying.
                                </p>
                              </div>
                              <div className="space-y-1">
                                <label className="flex items-center gap-2 text-sm">
                                  <Switch
                                    checked={domain.sync_generated_records}
                                    onCheckedChange={(checked) =>
                                      syncToggleMut.mutate({
                                        domain: domain.domain,
                                        enabled: checked,
                                      })
                                    }
                                    disabled={syncToggleMut.isPending}
                                  />
                                  Sync DNS records
                                </label>
                                <p className="pl-12 text-xs text-muted-foreground">
                                  Lets Temps create and update DNS records for
                                  generated project addresses in this zone.
                                  Existing custom domains are managed
                                  separately.
                                </p>
                              </div>
                              {provider.provider_type.toLowerCase() ===
                                'cloudflare' && (
                                <label className="flex items-center gap-2 text-sm">
                                  <Switch
                                    checked={domain.proxied_by_default}
                                    onCheckedChange={(checked) => {
                                      if (
                                        checked &&
                                        domain.generated_hostname_mode !==
                                          'flat'
                                      ) {
                                        toast.error(
                                          'Flat hostnames are required',
                                          {
                                            description:
                                              'Enable Flat hostnames first so Cloudflare Universal SSL covers generated records.',
                                          }
                                        )
                                        return
                                      }
                                      proxyToggleMut.mutate({
                                        domain: domain.domain,
                                        enabled: checked,
                                      })
                                    }}
                                    disabled={proxyToggleMut.isPending}
                                  />
                                  Proxy through Cloudflare
                                </label>
                              )}
                            </div>
                          )}
                          {domain.proxied_by_default && (
                            <p className="text-xs text-muted-foreground">
                              Public TLS terminates at Cloudflare; Temps serves
                              a self-signed origin certificate. Cloudflare Full
                              does not authenticate the origin. Full (strict)
                              needs a separately installed valid certificate for
                              the exact hostname or a matching wildcard.
                            </p>
                          )}
                        </div>
                        <div className="flex shrink-0 items-center gap-2">
                          {!domain.verified && (
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() =>
                                verifyDomainMut.mutate(domain.domain)
                              }
                              busy={verifyDomainMut.isPending}
                              busyLabel="Verifying…"
                            >
                              Verify
                            </Button>
                          )}
                          <Button
                            variant="ghost"
                            size="icon"
                            onClick={() => setDomainToRemove(domain)}
                          >
                            <Trash2 className="h-4 w-4" />
                          </Button>
                        </div>
                      </li>
                    ))}
                  </ul>
                )}
              </CardContent>
            </Card>
          </>
        }
        aside={
          <>
            {/* Credentials (masked) */}
            <Card>
              <CardHeader>
                <CardTitle>Credentials</CardTitle>
                <CardDescription>
                  Stored credentials for this provider (masked for security)
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="grid gap-4">
                  {Object.entries(
                    provider.credentials as Record<string, unknown>
                  ).map(([key, value]) => (
                    <div key={key} className="space-y-1">
                      <p className="text-sm font-medium">{key}</p>
                      <p className="text-sm text-muted-foreground font-mono">
                        {String(value)}
                      </p>
                    </div>
                  ))}
                </div>
              </CardContent>
            </Card>

            {/* Zones */}
            {zonesError && (
              <Callout tone="error" title="Available zones could not be loaded">
                {deliveryError(zonesQueryError)}{' '}
                <Button variant="link" onClick={() => refetchZones()}>
                  Retry
                </Button>
              </Callout>
            )}
            {zones && zones.zones.length > 0 && (
              <Card>
                <CardHeader>
                  <CardTitle>Available Zones</CardTitle>
                  <CardDescription>
                    DNS zones available in this provider account
                  </CardDescription>
                </CardHeader>
                <CardContent>
                  <ul role="list" className="divide-y rounded-md border">
                    {zones.zones.map((zone) => (
                      <li
                        key={zone.id}
                        className="flex items-center justify-between gap-3 px-3 py-2.5"
                      >
                        <div className="min-w-0">
                          <p className="truncate font-medium">{zone.name}</p>
                          <p className="truncate text-sm text-muted-foreground">
                            ID: {zone.id}
                          </p>
                        </div>
                        <Badge variant="outline" className="shrink-0">
                          {zone.status}
                        </Badge>
                      </li>
                    ))}
                  </ul>
                </CardContent>
              </Card>
            )}
          </>
        }
      />

      {/* Hostname mode preview / confirm dialog */}
      <Dialog
        open={!!hostnamePreview}
        onOpenChange={(open) => {
          if (!open) closeHostnamePreview()
        }}
      >
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>
              Switch {hostnamePreview?.domain} to{' '}
              {hostnamePreview?.target === 'flat' ? 'Flat' : 'Standard'}{' '}
              hostnames
            </DialogTitle>
            <DialogDescription>
              This is a breaking change: generated hostnames are recomputed,
              routes reload, and certificates re-issue. Existing nested
              hostnames stop resolving. Custom domains are not affected.
            </DialogDescription>
          </DialogHeader>

          {hostnamePreview?.result.zone_access_ok === false && (
            <Callout tone="error" title="Token cannot access this zone">
              DNS records will not be synced until the provider token is granted
              access to the zone.
            </Callout>
          )}

          <div className="max-h-80 space-y-4 overflow-y-auto">
            <div>
              <p className="mb-1 text-sm font-medium">
                Hostname changes (
                {hostnamePreview?.result.hostname_changes.length ?? 0})
              </p>
              {hostnamePreview?.result.hostname_changes.length ? (
                <ul className="space-y-1 text-sm">
                  {hostnamePreview.result.hostname_changes.map((c, i) => (
                    <li key={i} className="font-mono text-xs">
                      {c.old} → {c.new}
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="text-sm text-muted-foreground">
                  No generated hostnames change.
                </p>
              )}
            </div>

            {hostnameConflicts.length > 0 && (
              <HostnameConflictList
                conflicts={hostnameConflicts}
                decisions={conflictDecisions}
                onDecide={(conflict, decision) =>
                  setConflictDecisions((current) => ({
                    ...current,
                    [conflictKey(conflict)]: decision,
                  }))
                }
                disabled={applyModeMut.isPending}
              />
            )}

            {hostnamePreview?.syncDns && (
              <div>
                <p className="mb-1 text-sm font-medium">
                  DNS record changes ({plannedChanges.length})
                </p>
                {plannedChanges.length ? (
                  <ul className="space-y-1 text-sm">
                    {plannedChanges.map((c, i) => (
                      <li key={i} className="font-mono text-xs">
                        {c.action} {c.record_type} {c.name}
                        {c.value ? ` → ${c.value}` : ''}
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="text-sm text-muted-foreground">
                    No DNS record changes.
                  </p>
                )}
              </div>
            )}
          </div>

          {applyModeMut.isError && (
            <Callout tone="error" title="Applying the hostname mode failed">
              {deliveryError(applyModeMut.error)} Preview again to review the
              zone’s current state before retrying.
            </Callout>
          )}

          <DialogFooter className="gap-2 sm:items-center">
            {undecidedConflicts.length > 0 && (
              <p className="text-sm text-muted-foreground sm:mr-auto">
                Adopt or skip {undecidedConflicts.length} more conflicting{' '}
                {undecidedConflicts.length === 1 ? 'record' : 'records'} to
                apply.
              </p>
            )}
            <Button variant="outline" onClick={closeHostnamePreview}>
              Cancel
            </Button>
            {applyModeMut.isError && hostnamePreview && (
              <Button
                variant="outline"
                onClick={() =>
                  previewModeMut.mutate({
                    domain: hostnamePreview.domain,
                    target: hostnamePreview.target,
                    syncDns: hostnamePreview.syncDns,
                  })
                }
                busy={previewModeMut.isPending}
                busyLabel="Previewing…"
              >
                Preview again
              </Button>
            )}
            <Button
              onClick={() =>
                hostnamePreview &&
                applyModeMut.mutate({
                  domain: hostnamePreview.domain,
                  target: hostnamePreview.target,
                  syncDns: hostnamePreview.syncDns,
                  decisions: conflictDecisionsRequest(
                    hostnameConflicts,
                    conflictDecisions
                  ),
                })
              }
              disabled={
                undecidedConflicts.length > 0 || previewModeMut.isPending
              }
              busy={applyModeMut.isPending}
              busyLabel="Applying…"
            >
              Apply change
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Edit Dialog */}
      <Dialog open={isEditDialogOpen} onOpenChange={setIsEditDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Edit DNS Provider</DialogTitle>
            <DialogDescription>Update the provider settings</DialogDescription>
          </DialogHeader>
          <Form {...editForm}>
            <form
              onSubmit={editForm.handleSubmit((data) =>
                updateProviderMut.mutate(data)
              )}
              className="space-y-4"
            >
              <FormField
                control={editForm.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={editForm.control}
                name="description"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Description</FormLabel>
                    <FormControl>
                      <Textarea {...field} />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={editForm.control}
                name="is_active"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                    <div className="space-y-0.5">
                      <FormLabel className="text-base">Active</FormLabel>
                      <FormDescription>
                        Enable or disable this provider
                      </FormDescription>
                    </div>
                    <FormControl>
                      <Switch
                        checked={field.value}
                        onCheckedChange={field.onChange}
                      />
                    </FormControl>
                  </FormItem>
                )}
              />

              {provider.provider_type.toLowerCase() === 'cloudflare' && (
                <FormField
                  control={addDomainForm.control}
                  name="proxied_by_default"
                  render={({ field }) => (
                    <FormItem className="flex flex-row items-center justify-between rounded-lg border border-orange-500/30 bg-orange-500/5 p-4">
                      <div className="space-y-0.5 pr-4">
                        <FormLabel className="text-base">
                          Proxy through Cloudflare
                        </FormLabel>
                        <FormDescription>
                          Creates proxied records, enables flat hostnames, and
                          uses self-signed origin TLS to avoid Let&apos;s
                          Encrypt rate limits. Cloudflare Full does not
                          authenticate this origin. Full (strict) needs a
                          separately installed valid exact or wildcard
                          certificate. This switch does not change Cloudflare
                          SSL mode.
                        </FormDescription>
                      </div>
                      <FormControl>
                        <Switch
                          checked={field.value}
                          onCheckedChange={field.onChange}
                        />
                      </FormControl>
                    </FormItem>
                  )}
                />
              )}

              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => setIsEditDialogOpen(false)}
                >
                  Cancel
                </Button>
                <Button
                  type="submit"
                  busy={updateProviderMut.isPending}
                  busyLabel="Saving…"
                >
                  Save Changes
                </Button>
              </DialogFooter>
            </form>
          </Form>
        </DialogContent>
      </Dialog>

      {/* Add managed zone dialog */}
      <Dialog
        open={isAddDomainDialogOpen}
        onOpenChange={setIsAddDomainDialogOpen}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Add managed zone</DialogTitle>
            <DialogDescription>
              Choose a DNS zone this provider account can access.
            </DialogDescription>
          </DialogHeader>
          <Form {...addDomainForm}>
            <form
              onSubmit={addDomainForm.handleSubmit((data) =>
                addDomainMut.mutate(data)
              )}
              className="space-y-4"
            >
              <FormField
                control={addDomainForm.control}
                name="domain"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>DNS zone</FormLabel>
                    <FormControl>
                      {providerSupportsZoneSelection ? (
                        <SearchableSelect
                          value={field.value}
                          onValueChange={field.onChange}
                          options={selectableZones.map((zone) => ({
                            value: zone.name,
                            label: zone.name,
                            keywords: zone.id,
                          }))}
                          placeholder={
                            zonesPending
                              ? 'Loading zones…'
                              : 'Select an available DNS zone'
                          }
                          searchPlaceholder="Search available zones…"
                          emptyText="No available zones found"
                          disabled={
                            zonesPending ||
                            zonesError ||
                            selectableZones.length === 0
                          }
                        />
                      ) : (
                        <Input placeholder="example.com" {...field} />
                      )}
                    </FormControl>
                    <FormDescription>
                      {providerSupportsZoneSelection
                        ? 'Choose a zone from the connected provider account. Zones already managed here are omitted.'
                        : 'Enter the DNS zone name (for example, example.com).'}
                    </FormDescription>
                    {providerSupportsZoneSelection && zonesError && (
                      <p className="text-sm text-destructive">
                        {deliveryError(zonesQueryError)}{' '}
                        <Button
                          type="button"
                          variant="link"
                          onClick={() => refetchZones()}
                        >
                          Retry
                        </Button>
                      </p>
                    )}
                    {providerSupportsZoneSelection &&
                      !zonesPending &&
                      !zonesError &&
                      zones?.zones.length === 0 && (
                        <p className="text-sm text-muted-foreground">
                          No accessible zones were returned. Check your API key
                          or token and make sure the account has DNS zones.
                        </p>
                      )}
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={addDomainForm.control}
                name="auto_manage"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                    <div className="space-y-0.5">
                      <FormLabel className="text-base">
                        Auto-manage DNS
                      </FormLabel>
                      <FormDescription>
                        Automatically create and update DNS records
                      </FormDescription>
                    </div>
                    <FormControl>
                      <Switch
                        checked={field.value}
                        onCheckedChange={field.onChange}
                      />
                    </FormControl>
                  </FormItem>
                )}
              />

              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => setIsAddDomainDialogOpen(false)}
                >
                  Cancel
                </Button>
                <Button
                  type="submit"
                  busy={addDomainMut.isPending}
                  busyLabel="Adding…"
                  disabled={
                    providerSupportsZoneSelection &&
                    (zonesPending ||
                      zonesError ||
                      selectableZones.length === 0 ||
                      !addDomainForm.watch('domain'))
                  }
                >
                  Add zone
                </Button>
              </DialogFooter>
            </form>
          </Form>
        </DialogContent>
      </Dialog>

      {/* Delete Provider Dialog */}
      <AlertDialog
        open={isDeleteDialogOpen}
        onOpenChange={setIsDeleteDialogOpen}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete DNS Provider</AlertDialogTitle>
            <AlertDialogDescription>
              Are you sure you want to delete &quot;{provider.name}&quot;? This
              action cannot be undone and will remove all managed zones
              associated with this provider.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={deleteProviderMut.isPending}
              onClick={() => deleteProviderMut.mutate()}
            >
              {deleteProviderMut.isPending ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Deleting...
                </>
              ) : (
                'Delete Provider'
              )}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Remove managed zone dialog */}
      <AlertDialog
        open={!!domainToRemove}
        onOpenChange={(open) => !open && setDomainToRemove(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Remove managed zone</AlertDialogTitle>
            <AlertDialogDescription>
              Are you sure you want to remove &quot;{domainToRemove?.domain}
              &quot; from this provider? DNS records will no longer be
              automatically managed.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={removeDomainMut.isPending}
              onClick={() =>
                domainToRemove && removeDomainMut.mutate(domainToRemove.domain)
              }
            >
              {removeDomainMut.isPending ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Removing...
                </>
              ) : (
                'Remove zone'
              )}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  )
}
