import { useQuery } from '@tanstack/react-query'
import { ShieldCheck } from 'lucide-react'
import { useEffect, useState } from 'react'

import { Badge } from '@/components/ui/badge'
import { Card } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { EmptyState } from '@/components/ui/empty-state'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Button } from '@/components/ui/button'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { formatLocalDateTime } from '@/lib/date'
import {
  errorCategoryLabel,
  listOnDemandCertsOptions,
  type OnDemandCertRow,
} from '@/lib/on-demand-certs'

const PAGE_SIZE = 20

type StatusVariant = 'default' | 'secondary' | 'success' | 'warning' | 'destructive'

interface StatusDisplay {
  label: string
  variant: StatusVariant
}

/**
 * Map a `domains.status` value to a display label + Badge variant. On-demand
 * statuses (`on_demand_pending` / `on_demand_issuing` / `on_demand_failed`) are
 * collapsed to their human-facing forms (pending / issuing / failed); `active`
 * is the healthy issued state. `null` means no domains row exists yet.
 */
function statusDisplay(status: string | null): StatusDisplay {
  switch (status) {
    case 'active':
      return { label: 'active', variant: 'success' }
    case 'on_demand_pending':
    case 'pending':
      return { label: 'pending', variant: 'warning' }
    case 'on_demand_issuing':
      return { label: 'issuing', variant: 'warning' }
    case 'on_demand_failed':
    case 'failed':
      return { label: 'failed', variant: 'destructive' }
    case null:
    case undefined:
      return { label: 'unknown', variant: 'secondary' }
    default:
      return { label: status, variant: 'secondary' }
  }
}

function outcomeDisplay(outcome: string): StatusDisplay {
  switch (outcome) {
    case 'issued':
      return { label: 'issued', variant: 'success' }
    case 'failed':
      return { label: 'failed', variant: 'destructive' }
    default:
      // skipped_* outcomes
      return { label: outcome.replace(/_/g, ' '), variant: 'secondary' }
  }
}

function boolLabel(value: boolean | null): string {
  if (value === null) return '—'
  return value ? 'Yes' : 'No'
}

export function Certificates() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [page, setPage] = useState(1)
  const [selected, setSelected] = useState<OnDemandCertRow | null>(null)

  useEffect(() => {
    setBreadcrumbs([{ label: 'Certificates' }])
  }, [setBreadcrumbs])

  usePageTitle('Certificates')

  const { data, isLoading } = useQuery(
    listOnDemandCertsOptions({ page, page_size: PAGE_SIZE })
  )

  const total = data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE))
  const certs = data?.certs ?? []
  const showEmptyState = !isLoading && certs.length === 0

  return (
    <div className="space-y-4">
      {/* Page header */}
      <div className="flex flex-col gap-1">
        <h1 className="text-2xl font-semibold tracking-tight">Certificates</h1>
        <p className="text-sm text-muted-foreground">
          On-demand TLS certificate issuance attempts. Each hostname routed
          through the proxy that needs HTTPS is certified on first request via
          Let&apos;s Encrypt HTTP-01; every attempt — successful or not — is
          recorded here.
        </p>
      </div>

      {/* Table */}
      <Card>
        <div className="overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Hostname</TableHead>
                <TableHead className="w-[120px]">Status</TableHead>
                <TableHead className="hidden md:table-cell w-[120px]">
                  Outcome
                </TableHead>
                <TableHead className="hidden lg:table-cell text-right w-[200px]">
                  Last attempt
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {isLoading ? (
                Array.from({ length: 6 }).map((_, i) => (
                  <TableRow key={i}>
                    <TableCell>
                      <Skeleton className="h-4 w-64" />
                    </TableCell>
                    <TableCell>
                      <Skeleton className="h-5 w-16" />
                    </TableCell>
                    <TableCell className="hidden md:table-cell">
                      <Skeleton className="h-5 w-16" />
                    </TableCell>
                    <TableCell className="hidden lg:table-cell text-right">
                      <Skeleton className="h-4 w-32 ml-auto" />
                    </TableCell>
                  </TableRow>
                ))
              ) : showEmptyState ? (
                <TableRow className="hover:bg-transparent">
                  <TableCell colSpan={4} className="p-0">
                    <EmptyState
                      icon={ShieldCheck}
                      title="No certificate attempts yet"
                      description="On-demand TLS attempts appear here once a hostname is routed through the proxy and requires HTTPS. Enable on-demand TLS in settings to start issuing certificates automatically."
                    />
                  </TableCell>
                </TableRow>
              ) : (
                certs.map((row) => {
                  const status = statusDisplay(row.status)
                  const outcome = outcomeDisplay(row.attempt.outcome)
                  return (
                    <TableRow
                      key={row.attempt.id}
                      className="cursor-pointer"
                      onClick={() => setSelected(row)}
                    >
                      <TableCell className="font-mono text-sm">
                        {row.hostname}
                      </TableCell>
                      <TableCell>
                        <Badge variant={status.variant}>{status.label}</Badge>
                      </TableCell>
                      <TableCell className="hidden md:table-cell">
                        <Badge variant={outcome.variant}>{outcome.label}</Badge>
                      </TableCell>
                      <TableCell className="hidden lg:table-cell text-right text-sm text-muted-foreground">
                        {formatLocalDateTime(row.attempt.created_at)}
                      </TableCell>
                    </TableRow>
                  )
                })
              )}
            </TableBody>
          </Table>
        </div>
      </Card>

      {/* Pagination */}
      {!showEmptyState && (
        <div className="flex items-center justify-between">
          <p className="text-sm text-muted-foreground">
            <span className="hidden sm:inline">
              {total} attempt{total === 1 ? '' : 's'} · page {page} / {totalPages}
            </span>
            <span className="sm:hidden">
              {page} / {totalPages}
            </span>
          </p>
          <div className="flex gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPage((p) => Math.max(1, p - 1))}
              disabled={page === 1 || isLoading}
            >
              Previous
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
              disabled={page >= totalPages || isLoading}
            >
              Next
            </Button>
          </div>
        </div>
      )}

      <CertificateDetailSheet
        row={selected}
        onOpenChange={(open) => {
          if (!open) setSelected(null)
        }}
      />
    </div>
  )
}

interface CertificateDetailSheetProps {
  row: OnDemandCertRow | null
  onOpenChange: (open: boolean) => void
}

function DetailField({
  label,
  children,
}: {
  label: string
  children: React.ReactNode
}) {
  return (
    <div className="space-y-1">
      <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        {label}
      </p>
      <div className="text-sm">{children}</div>
    </div>
  )
}

function CertificateDetailSheet({
  row,
  onOpenChange,
}: CertificateDetailSheetProps) {
  // Keep the Sheet mounted (open-prop controlled) per the no-conditional-mount
  // rule; render body only when a row is selected.
  const status = row ? statusDisplay(row.status) : null
  const outcome = row ? outcomeDisplay(row.attempt.outcome) : null
  const categoryLabel = row
    ? errorCategoryLabel(
        row.attempt.error_category,
        row.attempt.acme_response_status,
        row.backoff_until
      )
    : null

  return (
    <Sheet open={!!row} onOpenChange={onOpenChange}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-lg">
        {row && status && outcome && (
          <>
            <SheetHeader>
              <SheetTitle className="break-all font-mono text-base">
                {row.hostname}
              </SheetTitle>
              <SheetDescription>
                Most recent on-demand TLS issuance attempt and current
                certificate state.
              </SheetDescription>
            </SheetHeader>

            <div className="mt-6 space-y-5">
              <div className="flex flex-wrap items-center gap-2">
                <Badge variant={status.variant}>{status.label}</Badge>
                <Badge variant={outcome.variant}>{outcome.label}</Badge>
              </div>

              {categoryLabel && (
                <div className="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">
                  {categoryLabel}
                </div>
              )}

              <div className="grid grid-cols-2 gap-4">
                <DetailField label="Challenge served">
                  {boolLabel(row.attempt.challenge_served)}
                </DetailField>
                <DetailField label="ACME request sent">
                  {boolLabel(row.attempt.acme_request_sent)}
                </DetailField>
                <DetailField label="ACME response">
                  {row.attempt.acme_response_status ? (
                    <span className="break-all font-mono text-xs">
                      {row.attempt.acme_response_status}
                    </span>
                  ) : (
                    '—'
                  )}
                </DetailField>
                <DetailField label="Duration">
                  {typeof row.attempt.duration_ms === 'number'
                    ? `${row.attempt.duration_ms} ms`
                    : '—'}
                </DetailField>
                <DetailField label="Trigger">
                  <span className="font-mono text-xs">{row.attempt.trigger}</span>
                </DetailField>
                <DetailField label="Attempted">
                  {formatLocalDateTime(row.attempt.created_at)}
                </DetailField>
                {row.expiration_time && (
                  <DetailField label="Expires">
                    {formatLocalDateTime(row.expiration_time)}
                  </DetailField>
                )}
                {row.backoff_until && (
                  <DetailField label="Backoff until">
                    {formatLocalDateTime(row.backoff_until)}
                  </DetailField>
                )}
              </div>

              {row.attempt.error_chain && (
                <DetailField label="Error chain">
                  <div className="relative">
                    <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded-md bg-muted p-3 pr-10 font-mono text-xs">
                      {row.attempt.error_chain}
                    </pre>
                    <div className="absolute right-2 top-2">
                      <CopyButton value={row.attempt.error_chain} />
                    </div>
                  </div>
                </DetailField>
              )}

              <p className="text-xs text-muted-foreground">
                Diagnose from the CLI with{' '}
                <code className="rounded bg-muted px-1 py-0.5 font-mono">
                  temps domain cert-status -d {row.hostname}
                </code>
                .
              </p>
            </div>
          </>
        )}
      </SheetContent>
    </Sheet>
  )
}
