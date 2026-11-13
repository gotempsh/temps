// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The projects' internal addresses this sandbox may call (ADR 105), each
 * with the `curl` line to run inside it, by `<env>.<project>.temps.local`
 * and, when it has one, by `<environment subdomain>.temps.local`. A project allows a sandbox under
 * its Settings > Protection > Internal access; without that, the call gets
 * a 403 that says the same.
 */
import { useQuery } from '@tanstack/react-query'
import { Waypoints } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { listSandboxInternalAddressesOptions } from '@/api/client/@tanstack/react-query.gen'
import type { SandboxInternalAddressResponse } from '@/api/client/types.gen'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { problemDetail } from '@/lib/api-problem'

export const INTERNAL_ADDRESSES_PAGE_SIZE = 20

/** Where a project's sandbox callers are managed. */
export function internalAccessSettingsPath(projectSlug: string): string {
  return `/projects/${projectSlug}/settings/security#internal-access`
}

export function SandboxInternalAddressesCard({
  sandboxId,
  disabled,
}: {
  sandboxId: string
  /** A destroyed sandbox calls nothing. */
  disabled?: boolean
}) {
  const [page, setPage] = useState(1)
  const addresses = useQuery({
    ...listSandboxInternalAddressesOptions({
      path: { sandbox_id: sandboxId },
      query: { page, page_size: INTERNAL_ADDRESSES_PAGE_SIZE },
    }),
    enabled: !disabled,
  })
  const data = addresses.data
  const totalPages = data ? Math.ceil(data.total / data.page_size) : 0

  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          <Waypoints className="h-4 w-4" aria-hidden />
          Internal addresses this sandbox can call
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        {disabled ? (
          <p className="text-sm text-muted-foreground">
            A destroyed sandbox calls nothing; its grants were removed.
          </p>
        ) : addresses.isError ? (
          <ErrorAlert
            title="Could not load internal addresses"
            description={problemDetail(
              addresses.error,
              'The internal addresses could not be loaded.'
            )}
            retry={() => addresses.refetch()}
            retrying={addresses.isFetching}
          />
        ) : data === undefined ? (
          <div className="space-y-2" aria-busy="true">
            <Skeleton className="h-12 w-full" />
            <Skeleton className="h-12 w-full" />
          </div>
        ) : data.total === 0 ? (
          <div className="rounded-md border border-dashed p-4 text-sm">
            <p className="font-medium">No project lets this sandbox call it.</p>
            <p className="mt-1 text-muted-foreground [overflow-wrap:anywhere]">
              A call such as{' '}
              <code className="text-xs">
                curl -sS http://production.orders.temps.local/
              </code>{' '}
              gets a 403. Open the project&apos;s Settings &gt; Protection &gt;
              Internal access and add sandbox{' '}
              <code className="text-xs">{sandboxId}</code> as a caller.
            </p>
            <Link
              to="/projects"
              className="mt-2 inline-block font-medium underline underline-offset-4"
            >
              Choose a project
            </Link>
          </div>
        ) : (
          <>
            <ul
              className="divide-y rounded-md border"
              aria-label="Internal addresses"
            >
              {data.items.map((address) => (
                <AddressRow
                  key={`${address.grant_id}-${address.environment_id}`}
                  address={address}
                />
              ))}
            </ul>
            {totalPages > 1 && (
              <ResponsivePagination
                page={page}
                pageSize={data.page_size}
                total={data.total}
                totalPages={totalPages}
                onPageChange={setPage}
                ariaLabel="Internal address pages"
              />
            )}
          </>
        )}
      </CardContent>
    </Card>
  )
}

function AddressRow({ address }: { address: SandboxInternalAddressResponse }) {
  return (
    <li className="space-y-2 p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-sm font-medium [overflow-wrap:anywhere]">
          {address.project_name} · {address.environment_name}
        </p>
        <Link
          to={internalAccessSettingsPath(address.project_slug)}
          className="text-xs text-muted-foreground underline underline-offset-4"
        >
          Internal access
        </Link>
      </div>
      <CurlLine command={address.curl_command} hostname={address.hostname} />
      {address.alias_curl_command && address.alias_hostname && (
        <>
          <p className="text-xs text-muted-foreground">
            Or by the environment&apos;s subdomain:
          </p>
          <CurlLine
            command={address.alias_curl_command}
            hostname={address.alias_hostname}
          />
        </>
      )}
    </li>
  )
}

function CurlLine({
  command,
  hostname,
}: {
  command: string
  hostname: string
}) {
  return (
    <div className="flex items-center gap-2 rounded-md bg-muted px-3 py-2">
      <code className="min-w-0 flex-1 font-mono text-xs [overflow-wrap:anywhere]">
        {command}
      </code>
      <CopyButton
        value={command}
        minimal
        label={`Copy the curl command for ${hostname}`}
      />
    </div>
  )
}
