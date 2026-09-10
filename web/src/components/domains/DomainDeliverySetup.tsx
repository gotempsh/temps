// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  applyDomainDeliveryBinding,
  getEnvironments,
  listDeliveryProfiles,
  listDnsProviders,
  listManagedDomains,
  previewDomainDeliveryBinding,
  type DomainDeliveryPreviewResponse,
  type DomainDeliveryBindingResponse,
} from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
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
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, ArrowRight } from 'lucide-react'
import { useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import { deliveryError, requireDeliveryData } from './delivery-errors'
import { DeliveryProfileSelect } from './ProjectDeliverySettings'

const schema = z.object({
  hostname: z.string().trim().min(1, 'Enter a hostname'),
  environment: z.string().min(1, 'Choose an environment'),
  dnsProvider: z.string().min(1, 'Choose a DNS provider'),
  zone: z.string().min(1, 'Choose a managed zone'),
  origin: z.string().trim().min(1, 'Enter the public origin address'),
  profile: z.string(),
})
type SetupForm = z.infer<typeof schema>
const selectClass =
  'flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring'

export function DomainDeliverySetup({
  projectId,
  open,
  onOpenChange,
  initialHostname = '',
  initialEnvironmentId,
  initialBinding,
}: {
  projectId: number
  open: boolean
  onOpenChange: (open: boolean) => void
  initialHostname?: string
  initialEnvironmentId?: number
  initialBinding?: DomainDeliveryBindingResponse
}) {
  const client = useQueryClient()
  const [preview, setPreview] = useState<DomainDeliveryPreviewResponse | null>(
    null
  )
  const [adopt, setAdopt] = useState(false)
  const form = useForm<SetupForm>({
    resolver: zodResolver(schema),
    defaultValues: {
      hostname: initialHostname,
      environment: initialEnvironmentId ? String(initialEnvironmentId) : '',
      dnsProvider: initialBinding ? String(initialBinding.dns_provider_id) : '',
      zone: initialBinding?.zone ?? '',
      origin: initialBinding?.origin_target ?? '',
      profile: initialBinding ? String(initialBinding.delivery_profile_id) : '',
    },
  })
  const providerId = useWatch({ control: form.control, name: 'dnsProvider' })
  const profiles = useQuery({
    queryKey: ['delivery-profiles'],
    queryFn: async () => requireDeliveryData(await listDeliveryProfiles()),
    enabled: open,
  })
  const environments = useQuery({
    queryKey: ['delivery-environments', projectId],
    queryFn: async () =>
      requireDeliveryData(
        await getEnvironments({ path: { project_id: projectId } })
      ),
    enabled: open,
  })
  const providers = useQuery({
    queryKey: ['delivery-dns-providers'],
    queryFn: async () => requireDeliveryData(await listDnsProviders()),
    enabled: open,
  })
  const zones = useQuery({
    queryKey: ['delivery-zones', providerId],
    queryFn: async () =>
      requireDeliveryData(
        await listManagedDomains({ path: { id: Number(providerId) } })
      ),
    enabled: open && !!providerId,
  })
  const inspect = useMutation({
    mutationFn: async (values: SetupForm) =>
      requireDeliveryData(
        await previewDomainDeliveryBinding({
          path: { project_id: projectId },
          body: {
            hostname: values.hostname,
            environment_id: Number(values.environment),
            dns_provider_id: Number(values.dnsProvider),
            zone: values.zone,
            origin_target: values.origin,
            delivery_profile_id: values.profile ? Number(values.profile) : null,
          },
        })
      ),
    onSuccess: (result) => {
      setPreview(result)
      setAdopt(false)
    },
  })
  const apply = useMutation({
    mutationFn: async () => {
      if (!preview) throw new Error('Preview the setup before applying it.')
      return requireDeliveryData(
        await applyDomainDeliveryBinding({
          path: { project_id: projectId },
          body: {
            preview_id: preview.preview_id,
            adopt_records: adopt
              ? [
                  {
                    name: preview.record.name,
                    record_type: preview.record.record_type,
                  },
                ]
              : [],
          },
        })
      )
    },
    onError: () => {
      client.invalidateQueries({ queryKey: ['delivery-bindings', projectId] })
    },
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ['delivery-bindings', projectId] })
      client.invalidateQueries({
        queryKey: ['delivery-custom-domains', projectId],
      })
      client.invalidateQueries({
        predicate: (query) =>
          JSON.stringify(query.queryKey).includes(
            'listCustomDomainsForProject'
          ),
      })
      toast.success('Domain DNS configuration applied', {
        description: 'Verify public HTTPS before sending production traffic.',
      })
      onOpenChange(false)
    },
  })
  const pending = inspect.isPending || apply.isPending
  const loading =
    profiles.isPending || environments.isPending || providers.isPending
  const queryError = profiles.error ?? environments.error ?? providers.error
  return (
    <Dialog
      open={open}
      onOpenChange={(value) => {
        if (!pending) onOpenChange(value)
      }}
    >
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>
            {preview ? 'Review domain setup' : 'Configure domain delivery'}
          </DialogTitle>
          <DialogDescription>
            {preview
              ? 'Review the DNS record and routing change before applying. Ownership is checked again when you confirm.'
              : 'Connect a hostname to this project using a shared delivery profile. Preview the changes before updating DNS.'}
          </DialogDescription>
        </DialogHeader>
        <ol
          aria-label="Setup progress"
          className="mb-2 flex items-center gap-3 text-xs text-muted-foreground"
        >
          <li className={!preview ? 'font-medium text-foreground' : ''}>
            1. Configure
          </li>
          <ArrowRight className="size-3" />
          <li className={preview ? 'font-medium text-foreground' : ''}>
            2. Inspect & confirm
          </li>
          <ArrowRight className="size-3" />
          <li>3. Apply & verify</li>
        </ol>
        {preview ? (
          <div className="space-y-5">
            <div className="rounded-lg border p-4">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <h3 className="break-all font-medium">
                  {form.getValues('hostname')}
                </h3>
                <Badge variant="secondary">
                  {profiles.data?.find(
                    (profile) => profile.id === preview.profile_id
                  )?.name ?? preview.provider_kind}
                </Badge>
              </div>
              <dl className="mt-4 grid grid-cols-[auto_1fr] gap-x-5 gap-y-2 text-sm">
                <dt className="text-muted-foreground">Environment</dt>
                <dd>
                  {
                    environments.data?.find(
                      (environment) =>
                        String(environment.id) === form.getValues('environment')
                    )?.name
                  }
                </dd>
                <dt className="text-muted-foreground">Routing</dt>
                <dd>
                  {preview.routing.will_create_custom_domain
                    ? 'Create a project domain route'
                    : 'Use the existing project domain route'}
                </dd>
                <dt className="text-muted-foreground">Origin TLS</dt>
                <dd>Preserve existing certificates and renewal</dd>
                <dt className="text-muted-foreground">Preview expires</dt>
                <dd>{new Date(preview.expires_at).toLocaleString()}</dd>
              </dl>
            </div>
            <div className="overflow-x-auto rounded-lg border">
              <table className="w-full text-left text-sm">
                <caption className="sr-only">Proposed DNS record</caption>
                <thead className="bg-muted/40 text-xs text-muted-foreground">
                  <tr>
                    <th className="p-3">Name</th>
                    <th className="p-3">Type</th>
                    <th className="p-3">Destination</th>
                    <th className="p-3">Proxy</th>
                    <th className="p-3">Ownership</th>
                  </tr>
                </thead>
                <tbody>
                  <tr>
                    <td className="p-3 font-mono">{preview.record.name}</td>
                    <td className="p-3">{preview.record.record_type}</td>
                    <td className="p-3 font-mono">{preview.record.value}</td>
                    <td className="p-3">
                      {preview.record.proxied ? 'Enabled' : 'Disabled'}
                    </td>
                    <td className="p-3">
                      {preview.record.ownership_status.replace(/_/g, ' ')}
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
            {preview.record.requires_adoption && (
              <div className="rounded-lg border border-amber-500/40 bg-amber-500/5 p-4">
                <p className="text-sm font-medium">
                  This record is not managed by Temps
                </p>
                <p className="mt-1 text-sm text-muted-foreground">
                  Adopting it permits Temps to update its destination and manage
                  future changes. You can skip this setup to leave the record
                  untouched.
                </p>
                <label className="mt-4 flex cursor-pointer items-start gap-3 text-sm">
                  <Checkbox
                    checked={adopt}
                    onCheckedChange={(value) => setAdopt(value === true)}
                    disabled={pending}
                  />
                  <span>
                    Adopt {preview.record.record_type} record{' '}
                    <strong>{preview.record.name}</strong> in{' '}
                    {form.getValues('zone')}
                  </span>
                </label>
              </div>
            )}
            {preview.warnings.map((warning, index) => (
              <Alert key={index}>
                <AlertDescription>{warning}</AlertDescription>
              </Alert>
            ))}
            {apply.isError && (
              <Alert variant="destructive">
                <AlertTitle>Setup could not finish</AlertTitle>
                <AlertDescription>
                  {deliveryError(apply.error)} Check the domain’s recorded
                  status before retrying. If this preview is stale, go back and
                  create a new preview.
                </AlertDescription>
              </Alert>
            )}
            <DialogFooter className="gap-2">
              <Button
                variant="ghost"
                disabled={pending}
                onClick={() => {
                  setPreview(null)
                  apply.reset()
                }}
              >
                <ArrowLeft className="mr-2 size-4" />
                Back
              </Button>
              <Button
                variant="outline"
                disabled={pending}
                onClick={() => onOpenChange(false)}
              >
                Skip setup
              </Button>
              <Button
                disabled={
                  pending || (preview.record.requires_adoption && !adopt)
                }
                onClick={() => apply.mutate()}
              >
                {apply.isPending
                  ? 'Applying and verifying…'
                  : 'Confirm and apply'}
              </Button>
            </DialogFooter>
          </div>
        ) : loading ? (
          <div className="space-y-4">
            <Skeleton className="h-16 w-full" />
            <Skeleton className="h-16 w-full" />
            <Skeleton className="h-16 w-full" />
          </div>
        ) : queryError ? (
          <Alert variant="destructive">
            <AlertDescription>
              {deliveryError(queryError)}{' '}
              <Button
                variant="link"
                onClick={() => {
                  profiles.refetch()
                  environments.refetch()
                  providers.refetch()
                }}
              >
                Retry
              </Button>
            </AlertDescription>
          </Alert>
        ) : (
          <Form {...form}>
            <form
              className="space-y-5"
              onSubmit={form.handleSubmit((values) => inspect.mutate(values))}
            >
              {(!profiles.data?.length || !providers.data?.length) && (
                <Alert>
                  <AlertTitle>Finish the shared setup</AlertTitle>
                  <AlertDescription>
                    Connect a{' '}
                    <Link className="underline" to="/dns-providers">
                      DNS provider
                    </Link>{' '}
                    and create a{' '}
                    <Link className="underline" to="/delivery-profiles">
                      delivery profile
                    </Link>{' '}
                    before configuring this domain.
                  </AlertDescription>
                </Alert>
              )}
              <div className="grid gap-4 sm:grid-cols-2">
                <FormField
                  control={form.control}
                  name="hostname"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Hostname</FormLabel>
                      <FormControl>
                        <Input
                          placeholder="shop.example.com"
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
                  name="environment"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Environment</FormLabel>
                      <FormControl>
                        <select className={selectClass} {...field}>
                          <option value="">Choose environment</option>
                          {environments.data?.map((environment) => (
                            <option
                              key={environment.id}
                              value={String(environment.id)}
                            >
                              {environment.name}
                            </option>
                          ))}
                        </select>
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </div>
              <FormField
                control={form.control}
                name="origin"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Public origin address</FormLabel>
                    <FormControl>
                      <Input
                        placeholder="Your server’s public IPv4 or IPv6 address"
                        autoComplete="off"
                        {...field}
                      />
                    </FormControl>
                    <p className="text-xs text-muted-foreground">
                      Confirm the address of the Temps server serving this
                      project.
                    </p>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <div className="grid gap-4 sm:grid-cols-2">
                <FormField
                  control={form.control}
                  name="dnsProvider"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>DNS provider</FormLabel>
                      <FormControl>
                        <select
                          className={selectClass}
                          {...field}
                          onChange={(event) => {
                            field.onChange(event)
                            form.setValue('zone', '')
                          }}
                        >
                          <option value="">Choose provider</option>
                          {providers.data
                            ?.filter((provider) => provider.is_active)
                            .map((provider) => (
                              <option
                                key={provider.id}
                                value={String(provider.id)}
                              >
                                {provider.name}
                              </option>
                            ))}
                        </select>
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="zone"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Managed zone</FormLabel>
                      <FormControl>
                        <select
                          className={selectClass}
                          disabled={!providerId || zones.isPending}
                          {...field}
                        >
                          <option value="">
                            {providerId && zones.isPending
                              ? 'Loading zones…'
                              : 'Choose zone'}
                          </option>
                          {zones.data?.map((zone) => (
                            <option key={zone.id} value={zone.domain}>
                              {zone.domain}
                            </option>
                          ))}
                        </select>
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </div>
              {zones.isError && (
                <Alert variant="destructive">
                  <AlertDescription>
                    {deliveryError(zones.error)}{' '}
                    <Button variant="link" onClick={() => zones.refetch()}>
                      Retry
                    </Button>
                  </AlertDescription>
                </Alert>
              )}
              {!!providerId && zones.isSuccess && !zones.data.length && (
                <p className="text-sm text-muted-foreground">
                  This provider has no managed zones.{' '}
                  <Link
                    className="underline"
                    to={`/dns-providers/${providerId}`}
                  >
                    Add a managed zone
                  </Link>{' '}
                  to continue.
                </p>
              )}
              <FormField
                control={form.control}
                name="profile"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Delivery profile</FormLabel>
                    <FormControl>
                      <DeliveryProfileSelect
                        profiles={profiles.data ?? []}
                        value={field.value}
                        onChange={field.onChange}
                        inheritLabel="Inherit environment / project default"
                      />
                    </FormControl>
                    <p className="text-xs text-muted-foreground">
                      The preview shows which profile will be applied to this
                      hostname.
                    </p>
                    <FormMessage />
                  </FormItem>
                )}
              />
              {inspect.isError && (
                <Alert variant="destructive">
                  <AlertTitle>Preview unavailable</AlertTitle>
                  <AlertDescription>
                    {deliveryError(inspect.error)}
                  </AlertDescription>
                </Alert>
              )}
              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  disabled={pending}
                  onClick={() => onOpenChange(false)}
                >
                  Cancel
                </Button>
                <Button
                  type="submit"
                  disabled={
                    pending ||
                    !profiles.data?.length ||
                    !providers.data?.length ||
                    !environments.data?.length
                  }
                >
                  {inspect.isPending ? 'Inspecting DNS…' : 'Preview setup'}
                  <ArrowRight className="ml-2 size-4" />
                </Button>
              </DialogFooter>
            </form>
          </Form>
        )}
      </DialogContent>
    </Dialog>
  )
}
