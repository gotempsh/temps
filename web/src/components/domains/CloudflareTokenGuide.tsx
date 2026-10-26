// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The least-privilege Cloudflare API token Temps needs, as the operator
// creates it in the Cloudflare dashboard. Temps only lists zones, edits DNS
// records and, for tunnels, manages the account's Cloudflare Tunnels.

import { useMutation } from '@tanstack/react-query'
import { CheckCircle2, CircleHelp, ExternalLink, XCircle } from 'lucide-react'
import { useEffect } from 'react'
import type { PermissionCheck, TokenCheckResult } from '@/api/client'
import { checkCloudflareTokenMutation } from '@/api/client/@tanstack/react-query.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { useDebounce } from '@/hooks/useDebounce'
import { cn } from '@/lib/utils'

export const CLOUDFLARE_API_TOKENS_URL =
  'https://dash.cloudflare.com/profile/api-tokens'

/**
 * Cloudflare's "create token" form with Zone · Zone · Read and Zone · DNS ·
 * Edit already filled in (ADR 064 D8). Cloudflare documents no key for
 * Account · Cloudflare Tunnel · Edit, so that one is added by hand.
 */
export const CLOUDFLARE_TOKEN_TEMPLATE_URL = `${CLOUDFLARE_API_TOKENS_URL}?permissionGroupKeys=${encodeURIComponent(
  JSON.stringify([
    { key: 'zone', type: 'read' },
    { key: 'dns', type: 'edit' },
  ])
)}&accountId=*&zoneId=all&name=Temps`

/** The token's permissions, in Cloudflare's own words, and what each is for. */
export const CLOUDFLARE_TOKEN_PERMISSIONS = [
  { permission: 'Zone · Zone · Read', purpose: 'find your domains' },
  { permission: 'Zone · DNS · Edit', purpose: 'create and remove DNS records' },
  {
    permission: 'Account · Cloudflare Tunnel · Edit',
    purpose: 'create the tunnel and run it on your workers',
    tunnelOnly: true,
  },
] as const

/**
 * What Account · Cloudflare Tunnel · Edit really grants (ADR 064 D8):
 * Cloudflare has no narrower form of it, so a stored token can change every
 * tunnel in the account. Shown next to the permission, before the token is
 * saved.
 */
export const TUNNEL_SCOPE_NOTE =
  "Covers every tunnel in this Cloudflare account, not only the ones Temps creates: if this token leaks, all of them are exposed. If other tunnels live in this account, use a separate Cloudflare account for Temps. When the control plane has a fixed outgoing IP address, you can also limit the token to it with Cloudflare's optional Client IP Address Filtering."

export function TunnelScopeNote({ className }: { className?: string }) {
  return (
    <p className={cn('text-xs text-amber-700 dark:text-amber-400', className)}>
      {TUNNEL_SCOPE_NOTE}
    </p>
  )
}

/**
 * What the token must have, at a glance: one line per permission and
 * resource. `CloudflareTokenGuide` walks through creating it.
 */
export function CloudflareTokenChecklist({
  tunnel = 'required',
  className,
}: {
  /** Whether the token must also manage Cloudflare Tunnels. */
  tunnel?: 'required' | 'optional'
  className?: string
}) {
  return (
    <div
      className={cn(
        'space-y-2 rounded-md border bg-muted/30 p-3 text-xs',
        className
      )}
    >
      <p className="text-sm font-medium">The Cloudflare API token needs</p>
      <div>
        <p className="font-medium text-muted-foreground">Permissions</p>
        <ul className="mt-1 space-y-1">
          {CLOUDFLARE_TOKEN_PERMISSIONS.map((entry) => (
            <li key={entry.permission} className="flex flex-wrap gap-x-2">
              <code className="rounded bg-muted px-1">{entry.permission}</code>
              <span className="text-muted-foreground">
                {'tunnelOnly' in entry && tunnel === 'optional'
                  ? 'only if workers take traffic through a Cloudflare Tunnel'
                  : `to ${entry.purpose}`}
              </span>
              {'tunnelOnly' in entry && <TunnelScopeNote className="w-full" />}
            </li>
          ))}
        </ul>
      </div>
      <div>
        <p className="font-medium text-muted-foreground">Limited to</p>
        <ul className="mt-1 list-disc space-y-0.5 pl-4 text-muted-foreground">
          <li>
            <strong className="text-foreground">Account Resources</strong>: the
            one account that holds your domain
          </li>
          <li>
            <strong className="text-foreground">Zone Resources</strong>: only
            the domains Temps manages, not &ldquo;All zones&rdquo;
          </li>
        </ul>
      </div>
      <a
        className="inline-flex items-center gap-0.5 text-foreground underline"
        href={CLOUDFLARE_TOKEN_TEMPLATE_URL}
        target="_blank"
        rel="noreferrer"
      >
        Create it in Cloudflare
        <ExternalLink className="h-3 w-3" />
      </a>
      <p className="text-muted-foreground">
        The link fills in both Zone permissions
        {tunnel === 'required'
          ? '; add Account · Cloudflare Tunnel · Edit yourself.'
          : '.'}
      </p>
    </div>
  )
}

const permissionIcons = {
  present: CheckCircle2,
  missing: XCircle,
  unverified: CircleHelp,
} as const

const permissionTone = {
  present: 'text-emerald-700 dark:text-emerald-400',
  missing: 'text-destructive',
  unverified: 'text-amber-700 dark:text-amber-400',
} as const

/** Each permission a checked token has or lacks, and what to change. */
export function TokenPermissionList({
  result,
  tunnel = 'required',
}: {
  result: TokenCheckResult
  tunnel?: 'required' | 'optional'
}) {
  if (!result.valid)
    return (
      <p role="status" className="text-xs text-destructive">
        {result.error ?? 'Cloudflare did not accept this token.'}
      </p>
    )
  const shown = result.permissions.filter(
    (p: PermissionCheck) =>
      tunnel === 'required' || p.permission !== 'tunnel_edit'
  )
  return (
    <ul role="status" className="space-y-1 text-xs">
      {shown.map((check) => {
        const Icon = permissionIcons[check.state]
        return (
          <li key={check.permission} className="flex gap-2">
            <Icon
              aria-hidden="true"
              className={cn(
                'mt-0.5 size-3.5 shrink-0',
                permissionTone[check.state]
              )}
            />
            <span>
              <code className="rounded bg-muted px-1">{check.label}</code>{' '}
              <span className="sr-only">{check.state}</span>
              {check.state !== 'present' && (
                <span className="text-muted-foreground">{check.detail}</span>
              )}
              {check.permission === 'tunnel_edit' && (
                <TunnelScopeNote className="mt-0.5" />
              )}
            </span>
          </li>
        )
      })}
      {result.scope && !result.scope.ok && (
        <li className="flex gap-2">
          <XCircle
            aria-hidden="true"
            className="mt-0.5 size-3.5 shrink-0 text-destructive"
          />
          <span>
            <span className="font-medium">
              Reaches more than {result.scope.zone}.
            </span>{' '}
            <span className="text-muted-foreground">{result.scope.detail}</span>
          </span>
        </li>
      )}
    </ul>
  )
}

/**
 * Checks a pasted token as soon as it is pasted and marks each permission
 * present or missing. The token is sent to Temps for the check only.
 */
export function CloudflareTokenCheck({
  token,
  zone,
  tunnel = 'required',
}: {
  token: string
  zone?: string | null
  tunnel?: 'required' | 'optional'
}) {
  const pasted = useDebounce(token.trim(), 400)
  const debouncedZone = useDebounce(zone?.trim() || null, 400)
  const check = useMutation(checkCloudflareTokenMutation())
  const { mutate } = check
  useEffect(() => {
    if (pasted.length >= 20)
      mutate({ body: { token: pasted, zone: debouncedZone } })
  }, [pasted, debouncedZone, mutate])
  if (pasted.length < 20) return null
  if (check.isPending) return <Skeleton className="h-12 w-full" />
  if (check.isError)
    return (
      <p role="status" className="text-xs text-muted-foreground">
        Temps could not check the token now; it is checked again when you
        continue.
      </p>
    )
  if (!check.data) return null
  return <TokenPermissionList result={check.data} tunnel={tunnel} />
}

export function CloudflareTokenGuide({
  tunnel = 'optional',
}: {
  /** Whether the token must also manage Cloudflare Tunnels. */
  tunnel?: 'required' | 'optional'
}) {
  return (
    <div className="space-y-2 rounded-md border p-3 text-sm">
      <p className="font-medium">Create a token with only what Temps needs</p>
      <ol className="list-decimal space-y-1.5 pl-5 text-muted-foreground">
        <li>
          Open{' '}
          <a
            className="inline-flex items-center gap-0.5 text-foreground underline"
            href={CLOUDFLARE_API_TOKENS_URL}
            target="_blank"
            rel="noreferrer"
          >
            API Tokens
            <ExternalLink className="h-3 w-3" />
          </a>{' '}
          in Cloudflare, then <strong>Create Token</strong> →{' '}
          <strong>Create Custom Token</strong>, or{' '}
          <a
            className="inline-flex items-center gap-0.5 text-foreground underline"
            href={CLOUDFLARE_TOKEN_TEMPLATE_URL}
            target="_blank"
            rel="noreferrer"
          >
            start from the filled-in form
            <ExternalLink className="h-3 w-3" />
          </a>
          . Don&apos;t use the Global API Key: it can do anything on your
          account.
        </li>
        <li>
          Permissions:
          <ul className="mt-1 list-disc space-y-0.5 pl-5">
            <li>
              <code>Zone · Zone · Read</code>, to find your domains
            </li>
            <li>
              <code>Zone · DNS · Edit</code>, to create and remove DNS records
            </li>
            <li>
              <code>Account · Cloudflare Tunnel · Edit</code>
              {tunnel === 'required'
                ? ', to create the tunnel and run it on your workers'
                : ', only if workers will take traffic through a Cloudflare Tunnel'}
              <TunnelScopeNote className="mt-0.5" />
            </li>
          </ul>
        </li>
        <li>
          <strong>Account Resources</strong>: Include → the one account that
          holds your domain.
        </li>
        <li>
          <strong>Zone Resources</strong>: Include → Specific zone → each domain
          Temps manages. Not &ldquo;All zones&rdquo;.
        </li>
        <li>
          Leave the expiry empty, or note it: when the token expires, Temps can
          no longer update DNS records
          {tunnel === 'required' ? ' or tunnels' : ''}.
        </li>
      </ol>
      <p className="text-xs text-muted-foreground">
        An existing token can be edited in Cloudflare to add a permission or a
        zone; it keeps its value, so Temps needs no change.
      </p>
    </div>
  )
}
