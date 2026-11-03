// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Info } from 'lucide-react'
import { Link } from 'react-router'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'

/**
 * Keeps a run of redeploys in view on the upload page. An upload deploys one
 * environment, so when several need the same change this says which one this
 * upload is for and which are still waiting, and confirms each deploy without
 * leaving the page while others remain.
 */
export function UploadQueueNotice({
  started,
  next,
  after,
  pending,
}: {
  /** The deploy just started, while environments are still waiting. */
  started: { environmentName: string; href: string } | null
  /** The environment the next upload goes to. */
  next: { name: string } | undefined
  /** The environments still waiting after that one. */
  after: readonly { name: string }[]
  /** Whether this upload is part of a run of redeploys at all. */
  pending: boolean
}) {
  if (!pending || !next) return null
  return (
    <Alert role="status">
      <Info className="size-4" />
      <AlertTitle>
        {started
          ? `Deployment to ${started.environmentName} started`
          : `Redeploying ${after.length + 1} environments`}
      </AlertTitle>
      <AlertDescription>
        <p>
          {started
            ? `${next.name} still needs its redeploy. Your files are still selected, so deploy them to ${next.name}, or clear them to upload something else.`
            : `An upload deploys one environment, so this starts with ${next.name}.`}
        </p>
        {after.length > 0 && (
          <p>
            Still to redeploy after that:{' '}
            {after.map(({ name }) => name).join(', ')}.
          </p>
        )}
        {started && (
          <div className="mt-3">
            <Button size="sm" variant="outline" asChild>
              <Link to={started.href}>View deployment</Link>
            </Button>
          </div>
        )}
      </AlertDescription>
    </Alert>
  )
}
