// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Callout, Status } from '@temps-sdk/ds'
import { Download, Loader2, Lock } from 'lucide-react'
import { toast } from 'sonner'
import { getManagedCaCertificate } from '@/api/client'
import {
  getServiceTlsOptions,
  getServiceTlsQueryKey,
  updateServiceTlsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ServerTlsMode,
  ServiceTlsStatusResponse,
} from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { ConfirmDialog } from '@/components/ui/confirm-dialog'
import { CopyButton } from '@/components/ui/copy-button'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { problemDetail } from '@/lib/api-problem'
import {
  confirmCopy,
  containerTlsLine,
  memberTlsLabel,
  tlsModeOptions,
  tlsVerdict,
  VERIFY_FULL_EXAMPLE,
} from '@/lib/service-tls'
import { cn } from '@/lib/utils'

function formatDate(iso: string): string {
  const date = new Date(iso)
  return Number.isNaN(date.getTime())
    ? iso
    : date.toLocaleDateString(undefined, {
        year: 'numeric',
        month: 'short',
        day: 'numeric',
      })
}

export interface ServiceTlsViewProps {
  serviceName: string
  status: ServiceTlsStatusResponse
  /** The mode being applied right now, if a change is in flight. */
  applyingMode?: ServerTlsMode | null
  downloading?: boolean
  onChooseMode: (mode: ServerTlsMode) => void
  onDownloadCa: () => void
}

/** What a managed database's TLS is set to and what it serves. Stateless. */
export function ServiceTlsView({
  serviceName,
  status,
  applyingMode = null,
  downloading = false,
  onChooseMode,
  onDownloadCa,
}: ServiceTlsViewProps) {
  const verdict = tlsVerdict(status)
  const busy = applyingMode !== null

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center gap-2">
        <Status tone={verdict.tone} label={verdict.label} />
        <span className="text-sm text-muted-foreground" role="status">
          {status.supported
            ? containerTlsLine(status)
            : `${serviceName} cannot offer TLS.`}
        </span>
      </div>

      {!status.supported ? (
        <Callout tone="info" title="TLS is not available for this database">
          {status.unsupported_reason ??
            'This database cannot be given a managed certificate.'}
        </Callout>
      ) : null}

      <div
        className="grid grid-cols-1 gap-3 md:grid-cols-3"
        role="group"
        aria-label="TLS mode"
      >
        {tlsModeOptions(status).map((option) => {
          const selected = status.mode === option.value
          const unavailable =
            option.unavailable ??
            (!status.supported && option.value !== 'off'
              ? 'This database cannot offer TLS.'
              : undefined)
          return (
            <button
              key={option.value}
              type="button"
              aria-pressed={selected}
              disabled={busy || unavailable !== undefined}
              onClick={() => {
                if (!selected) onChooseMode(option.value)
              }}
              className={cn(
                'rounded-lg border p-4 text-left transition-colors hover:bg-muted/50 disabled:cursor-not-allowed disabled:opacity-60',
                selected && 'border-primary bg-primary/5'
              )}
            >
              <div className="flex items-center gap-2 font-medium">
                {option.title}
                {applyingMode === option.value ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
                ) : null}
              </div>
              <p className="mt-1 text-sm text-muted-foreground">
                {option.description}
              </p>
              {unavailable ? (
                <p className="mt-2 text-xs text-muted-foreground">
                  {unavailable}
                </p>
              ) : null}
            </button>
          )
        })}
      </div>

      {status.members.length > 0 ? (
        <ul role="list" className="divide-y rounded-md border text-sm">
          {status.members.map((member) => (
            <li
              key={member.container_name}
              className="flex flex-col gap-1 p-3 sm:flex-row sm:items-center sm:justify-between"
            >
              <span className="font-mono text-xs [overflow-wrap:anywhere]">
                {member.container_name}
              </span>
              <span className="text-xs text-muted-foreground">
                {memberTlsLabel(member)}
              </span>
            </li>
          ))}
        </ul>
      ) : null}

      {status.mode === 'available' ? (
        <Callout
          tone="info"
          title="Only clients that ask for TLS are encrypted"
        >
          <p>
            Available mode does not force encryption. A client gets TLS only
            when it requests it, for example with <code>sslmode=require</code>.
            Add the CA below and use <code>sslmode=verify-full</code> to also
            check the server&apos;s identity:
          </p>
          <code className="mt-2 block rounded bg-muted px-2 py-1 font-mono text-xs [overflow-wrap:anywhere]">
            {VERIFY_FULL_EXAMPLE}
          </code>
          <p className="mt-2">
            Linked apps keep their current connection string.
          </p>
        </Callout>
      ) : null}

      {status.certificate ? (
        <dl className="grid grid-cols-1 gap-3 text-sm md:grid-cols-2">
          <div className="space-y-0.5">
            <dt className="text-muted-foreground">Certificate valid until</dt>
            <dd>{formatDate(status.certificate.not_after)}</dd>
          </div>
          <div className="space-y-0.5">
            <dt className="text-muted-foreground">Names</dt>
            <dd className="font-mono text-xs [overflow-wrap:anywhere]">
              {status.certificate.subject_alt_names.join(', ')}
            </dd>
          </div>
        </dl>
      ) : null}

      {status.certificate_authority ? (
        <div className="flex flex-col gap-3 rounded-lg border p-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0 space-y-1">
            <p className="text-sm font-medium">Temps managed services CA</p>
            <p className="text-xs text-muted-foreground">
              Valid until {formatDate(status.certificate_authority.not_after)}.
              SHA-256 fingerprint:
            </p>
            <p className="font-mono text-xs [overflow-wrap:anywhere]">
              {status.certificate_authority.fingerprint_sha256}
            </p>
          </div>
          <div className="flex shrink-0 gap-2">
            <CopyButton
              value={status.certificate_authority.fingerprint_sha256}
              label="Copy fingerprint"
              variant="outline"
              size="sm"
            />
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={downloading}
              onClick={onDownloadCa}
            >
              {downloading ? (
                <Loader2 className="h-4 w-4 animate-spin sm:mr-2" />
              ) : (
                <Download className="h-4 w-4 sm:mr-2" />
              )}
              <span className="max-sm:sr-only">Download CA certificate</span>
            </Button>
          </div>
        </div>
      ) : status.supported ? (
        <p className="text-sm text-muted-foreground">
          The CA certificate clients use to verify this database is created the
          first time TLS is turned on.
        </p>
      ) : null}
    </div>
  )
}

function downloadPem(fileName: string, pem: string) {
  const url = URL.createObjectURL(
    new Blob([pem], { type: 'application/x-pem-file' })
  )
  const link = document.createElement('a')
  link.href = url
  link.download = fileName
  link.click()
  URL.revokeObjectURL(url)
}

const TITLE = 'Connection encryption (TLS)'

/** A managed database's TLS setting, its certificate and the CA download. */
export function ServiceTlsCard({
  serviceId,
  serviceName,
}: {
  serviceId: number
  serviceName: string
}) {
  const queryClient = useQueryClient()
  const [confirmMode, setConfirmMode] = useState<ServerTlsMode | null>(null)
  const queryKey = getServiceTlsQueryKey({ path: { id: serviceId } })
  const statusQuery = useQuery(
    getServiceTlsOptions({ path: { id: serviceId } })
  )

  const update = useMutation({
    ...updateServiceTlsMutation(),
    onSuccess: (result) => {
      queryClient.setQueryData(queryKey, result.tls)
      void queryClient.invalidateQueries({ queryKey })
      toast.success(
        result.tls.mode === 'off'
          ? `TLS turned off for ${serviceName}`
          : `${serviceName} now offers TLS`
      )
    },
    onError: (error) => {
      void queryClient.invalidateQueries({ queryKey })
      toast.error('Could not change the TLS mode', {
        description: problemDetail(error, 'The TLS mode was not changed.'),
      })
    },
  })

  const download = useMutation({
    mutationFn: async () => {
      const { data } = await getManagedCaCertificate({
        path: { id: serviceId },
        throwOnError: true,
      })
      return data
    },
    onSuccess: (ca) => {
      downloadPem(ca.file_name, ca.certificate_pem)
      toast.success('CA certificate downloaded', {
        description: ca.file_name,
      })
    },
    onError: (error) =>
      toast.error('Could not download the CA certificate', {
        description: problemDetail(error, 'The CA certificate is unavailable.'),
      }),
  })

  const confirm = confirmMode
    ? confirmCopy(
        serviceName,
        confirmMode,
        statusQuery.data?.change_restarts_database ?? true
      )
    : null

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Lock className="h-4 w-4" />
          {TITLE}
        </CardTitle>
        <CardDescription>
          Encrypt connections to this database with a certificate Temps issues
          and renews.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <ConfirmDialog
          open={confirmMode !== null}
          onOpenChange={(open) => {
            if (!open) setConfirmMode(null)
          }}
          title={confirm?.title ?? ''}
          description={confirm?.description ?? ''}
          confirmLabel={confirm?.confirmLabel}
          destructive={confirmMode === 'off'}
          onConfirm={() => {
            if (confirmMode) {
              update.mutate({
                path: { id: serviceId },
                body: { mode: confirmMode },
              })
            }
            setConfirmMode(null)
          }}
        />
        {statusQuery.isPending ? (
          <div className="space-y-3" aria-busy="true">
            <Skeleton className="h-6 w-64" />
            <Skeleton className="h-24 w-full" />
            <Skeleton className="h-16 w-full" />
          </div>
        ) : statusQuery.isError ? (
          <ErrorAlert
            title="Could not load the TLS status"
            description={problemDetail(
              statusQuery.error,
              'The TLS status could not be loaded.'
            )}
            retry={() => statusQuery.refetch()}
            retrying={statusQuery.isFetching}
          />
        ) : (
          <ServiceTlsView
            serviceName={serviceName}
            status={statusQuery.data}
            applyingMode={update.isPending ? update.variables.body.mode : null}
            downloading={download.isPending}
            onChooseMode={setConfirmMode}
            onDownloadCa={() => download.mutate()}
          />
        )}
      </CardContent>
    </Card>
  )
}
