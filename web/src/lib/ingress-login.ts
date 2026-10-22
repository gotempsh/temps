// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { client } from '@/api/client/client.gen'

/**
 * The proxy's signed request to sign a visitor in to an environment that
 * requires a Temps login (ADR-055 §11), as it arrives in `/ingress/login`'s
 * query string. Mirrors `CreateIngressLoginGrantRequest` on the server.
 */
export interface IngressLoginRequest {
  owner_kind: string
  owner_id: number
  scheme: string
  host: string
  next?: string
  ts: number
  sig: string
}

/** Mirrors `IngressLoginGrantResponse`. */
export interface IngressLoginGrant {
  redirect_url: string
  grant_id: number
  expires_at: string
}

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

const BEARER_SECURITY = [{ scheme: 'bearer', type: 'http' }] as const

export async function createIngressLoginGrant(
  request: IngressLoginRequest
): Promise<IngressLoginGrant> {
  const { data } = await client.post<IngressLoginGrant, unknown, true>({
    security: [...BEARER_SECURITY],
    url: '/ingress/login-grants',
    body: request,
    headers: { 'Content-Type': 'application/json' },
    throwOnError: true,
  })
  return data
}
