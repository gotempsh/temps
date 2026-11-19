// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ExternalLink } from 'lucide-react'
import type { ServiceCatalogEntry } from '@/api/client'

/** Where to create the credential for a catalog service; shown under its input. */
export function ServiceCredentialHelp({
  entry,
}: {
  entry: Pick<ServiceCatalogEntry, 'name' | 'credential_help'>
}) {
  const help = entry.credential_help
  const url = help?.url
  return (
    <div className="space-y-1 text-sm text-muted-foreground">
      {url ? (
        <p>
          Create it in your {entry.name} account:{' '}
          <a
            href={url}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1 font-medium text-foreground underline underline-offset-4"
          >
            Open {entry.name} credentials
            <ExternalLink className="size-3.5" aria-hidden />
          </a>
        </p>
      ) : (
        <p>
          Create an API key or access token in your {entry.name} account&apos;s
          developer or API settings, then paste it here.
        </p>
      )}
      {help?.notes.map((note) => (
        <p key={note}>{note}</p>
      ))}
    </div>
  )
}
