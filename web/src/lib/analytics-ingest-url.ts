// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Shown in the snippet when the address could not be read. */
export const INGEST_URL_PLACEHOLDER = 'YOUR_TEMPS_URL'

export type IngestBaseUrlSource =
  /** The configured external URL. */
  | 'external_url'
  /** No external URL is stored; the address the console is open at. */
  | 'console_origin'
  /** Still loading, or the read failed: `YOUR_TEMPS_URL`, never a guess. */
  | 'placeholder'

export interface IngestBaseUrl {
  /** The absolute `basePath` to put in the install snippets. */
  url: string
  source: IngestBaseUrlSource
}

/**
 * The `basePath` of the analytics install snippets (ADR-040): an absolute URL,
 * because the request leaves the app's own origin entirely.
 *
 * It is the configured external URL whenever one is stored, for every role.
 * The page's own origin is only a stand-in for an instance with none stored,
 * and only once that is known: while the read is pending or after it failed
 * the snippet carries a placeholder, since presenting the address bar as the
 * endpoint is exactly what an app hosted elsewhere cannot use.
 */
export function analyticsIngestBaseUrl(input: {
  externalUrl: string | null | undefined
  /** `true` once the read finished, with or without a stored value. */
  isLoaded: boolean
  origin: string
}): IngestBaseUrl {
  const stored = input.externalUrl?.trim().replace(/\/+$/, '')
  if (stored) {
    return { url: `${stored}/api/_temps`, source: 'external_url' }
  }
  if (!input.isLoaded) {
    return {
      url: `${INGEST_URL_PLACEHOLDER}/api/_temps`,
      source: 'placeholder',
    }
  }
  return { url: `${input.origin}/api/_temps`, source: 'console_origin' }
}
