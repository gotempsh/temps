// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { lookupShieldClientOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ShieldLookupResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { ReadFailure } from '@/components/ui/read-failure'
import { Skeleton } from '@/components/ui/skeleton'
import { ipDetailPath } from '@/lib/ip-detail'
import { shieldModeLabel, shieldVerdictBadge } from '@/lib/shield'
import { zodResolver } from '@hookform/resolvers/zod'
import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { Loader2, Search } from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { z } from 'zod'
import type { ShieldEnvironmentPath } from './EnvironmentShieldView'
import { ShieldSection } from './ShieldSection'

const lookupSchema = z
  .object({
    ip: z.string().trim(),
    ja4: z.string().trim(),
  })
  .refine((value) => value.ip !== '' || value.ja4 !== '', {
    message: 'Enter an address, a JA4 fingerprint, or both.',
    path: ['ip'],
  })

type LookupForm = z.infer<typeof lookupSchema>

export function ShieldLookupPanel({ path }: { path: ShieldEnvironmentPath }) {
  // What was last submitted; the query runs only for a submitted lookup.
  const [submitted, setSubmitted] = useState<LookupForm | null>(null)
  const {
    register,
    handleSubmit,
    formState: { errors },
  } = useForm<LookupForm>({
    resolver: zodResolver(lookupSchema),
    defaultValues: { ip: '', ja4: '' },
  })

  const lookup = useQuery({
    ...lookupShieldClientOptions({
      path,
      query: {
        ip: submitted?.ip || undefined,
        ja4: submitted?.ja4 || undefined,
      },
    }),
    enabled: submitted !== null,
    // A lookup answers for the snapshot of the moment; keep it as asked.
    staleTime: 30_000,
  })

  return (
    <ShieldSection
      id="shield-lookup"
      title="Address lookup"
      description="Check what Shield would decide for a client address or TLS fingerprint in this environment, and why."
    >
      <form
        onSubmit={handleSubmit((values) => {
          // The same lookup again asks again: an operator entry changed
          // below should show in the answer.
          if (
            submitted &&
            submitted.ip === values.ip &&
            submitted.ja4 === values.ja4
          ) {
            void lookup.refetch()
          } else {
            setSubmitted(values)
          }
        })}
        className="grid gap-4 p-4 md:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] md:items-end"
        noValidate
      >
        <div className="min-w-0 space-y-2">
          <Label htmlFor="shield-lookup-ip">Address</Label>
          <Input
            id="shield-lookup-ip"
            placeholder="203.0.113.7"
            autoComplete="off"
            spellCheck={false}
            aria-invalid={errors.ip ? 'true' : undefined}
            aria-describedby={errors.ip ? 'shield-lookup-error' : undefined}
            {...register('ip')}
          />
        </div>
        <div className="min-w-0 space-y-2">
          <Label htmlFor="shield-lookup-ja4">JA4 fingerprint (optional)</Label>
          <Input
            id="shield-lookup-ja4"
            placeholder="t13d1516h2_8daaf6152771_02713d6af862"
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
            {...register('ja4')}
          />
        </div>
        <Button type="submit" disabled={lookup.isFetching}>
          {lookup.isFetching ? (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          ) : (
            <Search className="mr-2 h-4 w-4" />
          )}
          Look up
        </Button>
        {errors.ip && (
          <p
            id="shield-lookup-error"
            role="alert"
            className="text-sm text-destructive md:col-span-3"
          >
            {errors.ip.message}
          </p>
        )}
      </form>
      {submitted !== null && (
        <div className="border-t p-4">
          {lookup.error ? (
            <ReadFailure
              resource="Shield lookup"
              error={lookup.error}
              onRetry={() => lookup.refetch()}
              retrying={lookup.isFetching}
              embedded
            />
          ) : lookup.data ? (
            <LookupResult result={lookup.data} />
          ) : (
            <div role="status" aria-label="Looking up" className="space-y-2">
              <Skeleton className="h-5 w-40" />
              <Skeleton className="h-4 w-72 max-w-full" />
            </div>
          )}
        </div>
      )}
    </ShieldSection>
  )
}

function LookupResult({ result }: { result: ShieldLookupResponse }) {
  return (
    <div className="space-y-3" aria-live="polite">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-sm font-medium break-all">
          {result.ip ? (
            <Link
              to={ipDetailPath(result.ip)}
              className="underline-offset-4 hover:underline"
            >
              {result.ip}
            </Link>
          ) : null}
          {result.ip && result.ja4 ? ' with ' : null}
          {result.ja4}
        </span>
        <Badge
          variant={shieldVerdictBadge(result.allowed ? 'allow' : result.action)}
        >
          {result.allowed ? 'allowed' : result.action}
        </Badge>
      </div>
      <p className="text-sm text-muted-foreground">
        {result.allowed
          ? 'An operator allow entry matches, so this client passes whatever else it matches.'
          : result.action === 'pass'
            ? 'No list names this client.'
            : result.enforced
              ? `This environment is in ${shieldModeLabel(result.mode).toLowerCase()} mode, so the verdict is acted on.`
              : `This environment is in ${shieldModeLabel(result.mode).toLowerCase()} mode, so the verdict is only recorded.`}
      </p>
      {result.reasons.length > 0 && (
        <div>
          <h3 className="text-sm font-medium">Why</h3>
          <ul className="mt-1 space-y-1">
            {result.reasons.map((reason) => (
              <li key={reason} className="font-mono text-xs break-all">
                {reason}
              </li>
            ))}
          </ul>
        </div>
      )}
      <p className="text-xs text-muted-foreground">
        {result.snapshot_revision == null
          ? 'No snapshot is compiled yet; only the built-in lists were checked.'
          : `Snapshot revision ${result.snapshot_revision}.`}
      </p>
    </div>
  )
}
