// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createIngressLoginGrant as createIngressLoginGrantOperation } from '@/api/client'
import type {
  CreateIngressLoginGrantRequest,
  IngressLoginGrantResponse,
} from '@/api/client'

/**
 * The proxy's signed request to sign a visitor in to an environment that
 * requires a Temps login (ADR-055 §11), as it arrives in `/ingress/login`'s
 * query string.
 */
export type IngressLoginRequest = CreateIngressLoginGrantRequest

/** The grant the console mints, and where to send the browser with it. */
export type IngressLoginGrant = IngressLoginGrantResponse

/**
 * Reads the login request from the page's query string, or `null` when a
 * required field is missing or not a number.
 */
export function parseIngressLoginRequest(
  search: string
): IngressLoginRequest | null {
  const params = new URLSearchParams(search)
  const ownerKind = params.get('owner_kind')
  const ownerId = Number(params.get('owner_id'))
  const scheme = params.get('scheme')
  const host = params.get('host')
  const ts = Number(params.get('ts'))
  const sig = params.get('sig')
  if (
    !ownerKind ||
    !scheme ||
    !host ||
    !sig ||
    !Number.isSafeInteger(ownerId) ||
    !Number.isSafeInteger(ts)
  ) {
    return null
  }
  return {
    owner_kind: ownerKind,
    owner_id: ownerId,
    scheme,
    host,
    next: params.get('next') ?? undefined,
    ts,
    sig,
  }
}

export async function createIngressLoginGrant(
  request: IngressLoginRequest
): Promise<IngressLoginGrant> {
  const { data } = await createIngressLoginGrantOperation({
    body: request,
    throwOnError: true,
  })
  return data
}
