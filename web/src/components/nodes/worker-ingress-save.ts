// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { UpdatePublicIngressSettingsRequest } from '@/api/client/types.gen'

/** The listen-address choice that lets the worker pick its address. */
export const AUTOMATIC_LISTEN = '__automatic__'

/** The ingress form's values, as the operator edits them. */
export interface IngressDraft {
  enabled: boolean
  listen: string
  publicAddress: string
  httpPort: string
  httpsPort: string
}

/**
 * The save request for `draft`. `revision` is the one the settings were last
 * read with: the server refuses the save with 409 when someone else saved
 * since, instead of silently reverting their change (#2081).
 */
export function ingressSaveBody(
  draft: IngressDraft,
  revision: string | undefined
): UpdatePublicIngressSettingsRequest {
  return {
    enabled: draft.enabled,
    listen_address: draft.listen === AUTOMATIC_LISTEN ? null : draft.listen,
    public_address: draft.publicAddress.trim() || null,
    http_port: Number(draft.httpPort) || 80,
    https_port: Number(draft.httpsPort) || 443,
    expected_revision: revision ?? null,
  }
}
