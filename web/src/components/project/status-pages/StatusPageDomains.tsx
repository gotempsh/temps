// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  addStatusPageDomainMutation,
  listStatusPageDomainsOptions,
  removeStatusPageDomainMutation,
  renewStatusPageDomainMutation,
  verifyStatusPageDomainMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  DnsRecordInstruction,
  StatusPageDomainResponse,
  StatusPageResponse,
} from '@/api/client'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ExternalLink,
  Plus,
  RefreshCw,
  ShieldCheck,
  Trash2,
} from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import {
  addDomainSchema,
  CERTIFICATE_RENEWAL_WINDOW_DAYS,
  daysUntil,
  domainActions,
  domainStatusLabel,
  operationQueries,
  type AddDomainValues,
} from './status-page-form'

function StatusBadge({ domain }: { domain: StatusPageDomainResponse }) {
  const variant =
    domain.status === 'active'
      ? 'success-subtle'
      : domain.status === 'failed' || domain.status === 'renewal_failed'
        ? 'destructive-subtle'
        : 'warning-subtle'
  return <Badge variant={variant}>{domainStatusLabel(domain)}</Badge>
}

function DnsRecordsTable({ records }: { records: DnsRecordInstruction[] }) {
  return (
    <div className="rounded-md border">
      <Table className="min-w-[520px]">
        <TableHeader>
          <TableRow>
            <TableHead>Type</TableHead>
            <TableHead>Name</TableHead>
            <TableHead>Value</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {records.map((record) => (
            <TableRow
              key={`${record.record_type}-${record.name}`}
              className="align-top"
            >
              <TableCell className="font-mono">{record.record_type}</TableCell>
              <TableCell>
                <div className="flex items-center gap-1">
                  <code className="break-all text-xs">{record.name}</code>
                  <CopyButton
                    value={record.name}
                    variant="ghost"
                    size="sm"
                    minimal
                    label={`Copy ${record.record_type} record name`}
                  />
                </div>
              </TableCell>
              <TableCell>
                <div className="flex items-center gap-1">
                  <code className="break-all text-xs">{record.value}</code>
                  <CopyButton
                    value={record.value}
                    variant="ghost"
                    size="sm"
                    minimal
                    label={`Copy ${record.record_type} record value`}
                  />
                </div>
                <p className="mt-1 text-xs text-muted-foreground">
                  {record.purpose}
                </p>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  )
}

/** One line on what to do next, for every state but "serving and current". */
function domainGuidance(
  domain: StatusPageDomainResponse,
  pagePublished: boolean
): string {
  if (!pagePublished) {
    return 'Certificates are only issued for published pages. Publish the page, then verify.'
  }
  switch (domain.status) {
    case 'unverified':
      return 'No certificate yet. Create the DNS record below, then verify.'
    case 'renewal_pending':
      return domain.verification_method === 'dns-01'
        ? 'A renewal is in progress and the current certificate is still served. Publish the new TXT record below, then verify.'
        : 'A renewal is in progress and the current certificate is still served. Verify to finish it.'
    case 'renewal_failed':
      return 'The last renewal failed. The current certificate is still served until it expires; renew again.'
    default:
      return 'Create these DNS records at your DNS provider, wait for them to propagate, then verify. Nothing is served on this hostname until it is verified.'
  }
}

/** Expiry note for a domain that holds a certificate. */
function CertificateExpiry({ domain }: { domain: StatusPageDomainResponse }) {
  const days = daysUntil(domain.certificate_expires_at)
  if (days === null || !domain.certificate_expires_at) return null
  const dueSoon = days <= CERTIFICATE_RENEWAL_WINDOW_DAYS
  const expires = new Date(domain.certificate_expires_at).toLocaleDateString()
  return (
    <p
      className={
        dueSoon && domain.verification_method === 'dns-01'
          ? 'text-sm text-warning'
          : 'text-sm text-muted-foreground'
      }
    >
      Certificate valid until {expires} ({days} days).{' '}
      {domain.verification_method === 'dns-01'
        ? dueSoon
          ? 'DNS-verified certificates are not renewed automatically: renew now and publish the new TXT record.'
          : `DNS-verified certificates are not renewed automatically: renew within ${CERTIFICATE_RENEWAL_WINDOW_DAYS} days of expiry.`
        : `Renewed automatically within ${CERTIFICATE_RENEWAL_WINDOW_DAYS} days of expiry.`}
    </p>
  )
}

function DomainRow({
  page,
  domain,
}: {
  page: StatusPageResponse
  domain: StatusPageDomainResponse
}) {
  const queryClient = useQueryClient()
  const [confirmRemove, setConfirmRemove] = useState(false)
  const path = { path: { status_page_id: page.id, domain_id: domain.id } }
  const refresh = () =>
    queryClient.invalidateQueries(operationQueries('listStatusPageDomains'))
  const verify = useMutation({
    ...verifyStatusPageDomainMutation(),
    meta: { errorTitle: `Could not verify ${domain.hostname} yet` },
    onSuccess: async (result) => {
      await refresh()
      if (result.status === 'active') {
        toast.success(
          result.serving
            ? `${result.hostname} is verified and serving the page.`
            : `${result.hostname} is verified. It serves once the page is published.`
        )
      } else {
        toast.info(
          `Create the DNS records shown for ${result.hostname}, then verify again.`
        )
      }
    },
    // Not onError: a local onError replaces the app-wide error toast, and a
    // refusal the server did not store on the row would then go unannounced.
    onSettled: async (_result, error) => {
      if (error) await refresh()
    },
  })
  const renew = useMutation({
    ...renewStatusPageDomainMutation(),
    meta: {
      errorTitle: `Could not renew the certificate of ${domain.hostname}`,
    },
    onSuccess: async (result) => {
      await refresh()
      if (result.status === 'active') {
        toast.success(`Renewed the certificate of ${result.hostname}.`)
      } else {
        toast.info(
          `Publish the new TXT record for ${result.hostname}, then verify. The current certificate keeps serving meanwhile.`
        )
      }
    },
    // Not onError: a local onError replaces the app-wide error toast, and a
    // refusal the server did not store on the row would then go unannounced.
    onSettled: async (_result, error) => {
      if (error) await refresh()
    },
  })
  const actions = domainActions(domain, page.is_published)
  const busy = verify.isPending || renew.isPending
  const remove = useMutation({
    ...removeStatusPageDomainMutation(),
    meta: { errorTitle: `Failed to remove ${domain.hostname}` },
    onSuccess: async () => {
      await refresh()
      setConfirmRemove(false)
      toast.success(`${domain.hostname} removed. It now answers "not found".`)
    },
  })

  return (
    <div className="space-y-3 p-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 flex-wrap items-center gap-2">
          <span className="truncate font-medium">{domain.hostname}</span>
          <StatusBadge domain={domain} />
          <span className="text-xs text-muted-foreground">
            {domain.verification_method === 'dns-01'
              ? 'DNS verification'
              : 'HTTP verification'}
          </span>
        </div>
        <div className="flex gap-2">
          {domain.serving && (
            <Button variant="outline" size="sm" asChild>
              <a
                href={`https://${domain.hostname}/`}
                target="_blank"
                rel="noreferrer"
              >
                <ExternalLink className="size-4 sm:mr-2" />
                <span className="max-sm:sr-only">Open</span>
              </a>
            </Button>
          )}
          {actions.verify && (
            <Button
              size="sm"
              disabled={busy}
              onClick={() => verify.mutate(path)}
            >
              <ShieldCheck className="size-4 sm:mr-2" />
              <span className="max-sm:sr-only">
                {verify.isPending ? 'Verifying...' : 'Verify'}
              </span>
            </Button>
          )}
          {actions.renew && (
            <Button
              variant={
                domain.status === 'renewal_failed' ? 'default' : 'outline'
              }
              size="sm"
              disabled={busy}
              onClick={() => renew.mutate(path)}
            >
              <RefreshCw className="size-4 sm:mr-2" />
              <span className="max-sm:sr-only">
                {renew.isPending ? 'Renewing...' : 'Renew'}
              </span>
            </Button>
          )}
          <Button
            variant="outline"
            size="sm"
            aria-label={`Remove ${domain.hostname}`}
            onClick={() => setConfirmRemove(true)}
          >
            <Trash2 className="size-4" />
          </Button>
        </div>
      </div>

      {domain.status === 'active' && page.is_published ? (
        <CertificateExpiry domain={domain} />
      ) : (
        <div className="space-y-2">
          <p className="text-sm text-muted-foreground">
            {domainGuidance(domain, page.is_published)}
          </p>
          {(domain.status === 'renewal_pending' ||
            domain.status === 'renewal_failed') && (
            <CertificateExpiry domain={domain} />
          )}
          <DnsRecordsTable records={domain.dns_records} />
          {domain.last_error && (
            <p className="break-words text-sm text-destructive">
              Last attempt: {domain.last_error}
            </p>
          )}
        </div>
      )}

      <ConfirmDeleteDialog
        open={confirmRemove}
        onOpenChange={setConfirmRemove}
        verb="Remove"
        resourceType="status page domain"
        resourceName={domain.hostname}
        consequences="The hostname stops serving the page immediately and its certificate is deleted. It keeps answering “not found” until it is used again."
        isPending={remove.isPending}
        onConfirm={() => remove.mutate(path)}
      />
    </div>
  )
}

function AddDomainForm({ page }: { page: StatusPageResponse }) {
  const queryClient = useQueryClient()
  const form = useForm<AddDomainValues>({
    resolver: zodResolver(addDomainSchema),
    defaultValues: { hostname: '', verification_method: 'http-01' },
  })
  const add = useMutation({
    ...addStatusPageDomainMutation(),
    meta: { errorTitle: 'Could not add the domain' },
    onSuccess: async (domain) => {
      await queryClient.invalidateQueries(
        operationQueries('listStatusPageDomains')
      )
      form.reset()
      toast.success(
        `${domain.hostname} added. Create the DNS records shown, then verify.`
      )
    },
  })
  return (
    <Form {...form}>
      <form
        className="space-y-3"
        onSubmit={form.handleSubmit((values) =>
          add.mutate({ path: { status_page_id: page.id }, body: values })
        )}
      >
        <fieldset
          className="min-w-0 space-y-3"
          disabled={add.isPending}
          aria-busy={add.isPending}
        >
          <FormField
            control={form.control}
            name="hostname"
            render={({ field }) => (
              <FormItem>
                <FormLabel>Hostname</FormLabel>
                <FormControl>
                  <Input placeholder="status.example.com" {...field} />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name="verification_method"
            render={({ field }) => (
              <FormItem>
                <FormLabel>Prove you control it with</FormLabel>
                <FormControl>
                  <RadioGroup
                    value={field.value}
                    onValueChange={field.onChange}
                    className="grid gap-2 sm:grid-cols-2"
                  >
                    <label className="flex cursor-pointer items-start gap-3 rounded-md border p-3 text-sm">
                      <RadioGroupItem value="http-01" className="mt-0.5" />
                      <span>
                        <span className="font-medium">HTTP</span>
                        <span className="block text-muted-foreground">
                          The hostname already points at this Temps instance.
                        </span>
                      </span>
                    </label>
                    <label className="flex cursor-pointer items-start gap-3 rounded-md border p-3 text-sm">
                      <RadioGroupItem value="dns-01" className="mt-0.5" />
                      <span>
                        <span className="font-medium">DNS TXT record</span>
                        <span className="block text-muted-foreground">
                          Verify before switching traffic over.
                        </span>
                      </span>
                    </label>
                  </RadioGroup>
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <div className="flex justify-end">
            <Button type="submit" disabled={add.isPending}>
              <Plus className="mr-2 size-4" />
              {add.isPending ? 'Adding...' : 'Add domain'}
            </Button>
          </div>
        </fieldset>
      </form>
    </Form>
  )
}

/**
 * Custom hostnames for a status page. A hostname serves the page only after
 * ownership is verified (an ACME certificate is issued) and while the page is
 * published; everything else on it answers "not found".
 */
export function StatusPageDomains({ page }: { page: StatusPageResponse }) {
  const domains = useQuery(
    listStatusPageDomainsOptions({ path: { status_page_id: page.id } })
  )
  const items = domains.data ?? []

  return (
    <section className="space-y-3">
      <div>
        <h3 className="text-lg font-semibold">Custom domains</h3>
        <p className="text-sm text-muted-foreground">
          Serve this page on your own hostname, e.g.{' '}
          <code className="text-xs">status.example.com</code>. Temps checks that
          you control it and issues a certificate before serving anything there.
          Only the page and <code className="text-xs">/summary.json</code> are
          reachable on it.
          {!page.is_published &&
            ' Domains serve only while the page is published.'}
        </p>
      </div>
      {domains.isError ? (
        <ErrorAlert
          title="Failed to load custom domains"
          description={
            domains.error instanceof Error
              ? domains.error.message
              : 'An unexpected error occurred'
          }
          retry={() => domains.refetch()}
        />
      ) : domains.isLoading ? (
        <Skeleton className="h-20 w-full" />
      ) : (
        <Card>
          <CardContent className="divide-y p-0">
            {items.map((domain) => (
              <DomainRow key={domain.id} page={page} domain={domain} />
            ))}
            <div className="p-4">
              <AddDomainForm page={page} />
            </div>
          </CardContent>
        </Card>
      )}
    </section>
  )
}
