// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { AlertTriangle, ExternalLink } from 'lucide-react'
import { Link } from 'react-router'
import {
  describePublicLink,
  type PublicLinkSource,
  type PublicLinkState,
} from './public-link'

interface PublicLinkSectionProps {
  page: PublicLinkSource & { is_published: boolean }
}

function SetupLink({ setupPath }: { setupPath: string }) {
  return (
    <Link to={setupPath} className="font-medium underline underline-offset-4">
      Set the public URL
    </Link>
  )
}

/** Shown instead of the link while the install has no public address. */
function MissingPublicUrl({
  state,
}: {
  state: Extract<PublicLinkState, { kind: 'missing' }>
}) {
  return (
    <Alert variant="warning">
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>Public URL is not configured</AlertTitle>
      <AlertDescription className="space-y-2">
        <p>{state.reason}</p>
        <p>
          Once it is set, this page&apos;s link appears here, for example{' '}
          <code className="text-xs">
            https://temps.example.com/api/status/…
          </code>
          . Until then there is nothing to copy or open.
        </p>
        <p>
          <SetupLink setupPath={state.setupPath} />
        </p>
      </AlertDescription>
    </Alert>
  )
}

function PublicUrlWarning({
  state,
}: {
  state: Extract<PublicLinkState, { kind: 'warning' }>
}) {
  return (
    <Alert variant="warning">
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>This link may not open for visitors</AlertTitle>
      <AlertDescription className="space-y-2">
        <p>{state.reason}</p>
        <p>
          <SetupLink setupPath={state.setupPath} />
        </p>
      </AlertDescription>
    </Alert>
  )
}

function PublicLinkRow({
  url,
  isPublished,
}: {
  url: string
  isPublished: boolean
}) {
  return (
    <div className="flex min-w-0 flex-col gap-2 sm:flex-row sm:items-center">
      <code className="min-w-0 truncate rounded-md border bg-muted px-3 py-2 text-sm">
        {url}
      </code>
      <div className="flex gap-2">
        <CopyButton value={url} variant="outline" size="sm">
          Copy
        </CopyButton>
        {isPublished && (
          <Button variant="outline" size="sm" asChild>
            <a href={url} target="_blank" rel="noreferrer">
              <ExternalLink className="size-4 sm:mr-2" />
              <span className="max-sm:sr-only">Open</span>
            </a>
          </Button>
        )}
      </div>
    </div>
  )
}

/**
 * The "Public link" cell of the status pages table. A draft has no public
 * link yet; a published page offers Copy only when there is an address, and
 * otherwise points at the setting that provides one.
 */
export function PublicLinkCell({ page }: PublicLinkSectionProps) {
  if (!page.is_published) {
    return <span className="text-sm text-muted-foreground">Not public</span>
  }
  const state = describePublicLink(page)
  if (state.kind === 'missing') {
    return (
      <Link
        to={state.setupPath}
        className="text-sm text-muted-foreground underline underline-offset-4"
      >
        Public URL not configured
      </Link>
    )
  }
  return (
    <CopyButton
      value={state.url}
      variant="ghost"
      size="sm"
      label="Copy public link"
    >
      Copy link
    </CopyButton>
  )
}

/**
 * Always rendered: with a public address it offers Copy and Open, without one
 * it explains what is missing and links to the setting, so the link never
 * silently disappears and a broken one is never offered.
 */
export function PublicLinkSection({ page }: PublicLinkSectionProps) {
  const state = describePublicLink(page)

  return (
    <section className="space-y-2">
      <h3 className="text-lg font-semibold">Public link</h3>
      {state.kind === 'missing' ? (
        <MissingPublicUrl state={state} />
      ) : (
        <PublicLinkRow url={state.url} isPublished={page.is_published} />
      )}
      {state.kind === 'warning' && <PublicUrlWarning state={state} />}
      <p className="text-sm text-muted-foreground">
        {page.is_published
          ? 'Anyone with this link can see the page. Unpublish or rotate the link to revoke access immediately. Search engines are asked not to index it.'
          : 'This link returns “not found” until you publish the page.'}{' '}
        A machine-readable version is at{' '}
        <code className="text-xs">…/summary.json</code>.
      </p>
    </section>
  )
}
