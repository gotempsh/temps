// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createSandboxPreviewSession } from '@/api/client'
import { problemDetail } from '@/lib/api-problem'

/** The slice of the generated SDK call this module uses; tests fake it. */
export type CreatePreviewSession = (options: {
  body: { sandbox_id: string; url: string }
}) => Promise<{
  data?: { redirect_url: string }
  error?: unknown
  response?: Response
}>

/**
 * What to load in the frame that shows `url`, a page of sandbox `sandboxId`.
 *
 * A sandbox whose preview asks for a Temps login answers a bare frame with
 * a redirect to the console's sign-in page, which cannot be framed. The
 * console instead asks for a one-time preview session and frames its
 * `redirect_url`: it signs this browser in to the preview host and lands on
 * `url`. A 409 means the preview does not ask for a Temps login (public or
 * password-protected), so the page is framed as it is.
 *
 * Each session is single use: call this again for every load, including a
 * reload of the same page.
 */
export async function previewFrameSource(
  sandboxId: string,
  url: string,
  create: CreatePreviewSession = createSandboxPreviewSession as unknown as CreatePreviewSession
): Promise<string> {
  const { data, error, response } = await create({
    body: { sandbox_id: sandboxId, url },
  })
  if (response?.status === 409) return url
  if (error !== undefined || !data) {
    throw new Error(
      problemDetail(error, `Could not open a preview session for ${url}`)
    )
  }
  return data.redirect_url
}
