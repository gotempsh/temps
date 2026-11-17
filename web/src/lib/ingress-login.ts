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
 * required field is missing or malformed.
 *
 * `port` is present when the visitor reached the site on a non-default port
 * (the proxy signs `host:port`), and is passed through unchanged so the
 * grant sends them back to that port.
 */
export function parseIngressLoginRequest(
  search: string
): IngressLoginRequest | null {
  const params = new URLSearchParams(search)
  const ownerKind = params.get('owner_kind')
  const ownerId = integerParam(params, 'owner_id')
  const scheme = params.get('scheme')
  const host = params.get('host')
  const ts = integerParam(params, 'ts')
  const sig = params.get('sig')
  const rawPort = params.get('port')
  const port = rawPort === null ? undefined : integerParam(params, 'port')
  if (
    !ownerKind ||
    !scheme ||
    !host ||
    !sig ||
    ownerId === null ||
    ts === null ||
    port === null ||
    (port !== undefined && (port < 1 || port > 65535))
  ) {
    return null
  }
  return {
    owner_kind: ownerKind,
    owner_id: ownerId,
    scheme,
    host,
    ...(port === undefined ? {} : { port }),
    next: params.get('next') ?? undefined,
    ts,
    sig,
  }
}

/** The site as the visitor reached it: `host`, or `host:port`. */
export function ingressLoginAuthority(request: IngressLoginRequest): string {
  return request.port ? `${request.host}:${request.port}` : request.host
}

function integerParam(params: URLSearchParams, name: string): number | null {
  const raw = params.get(name)
  if (raw === null || !/^[0-9]+$/.test(raw)) return null
  const value = Number(raw)
  return Number.isSafeInteger(value) ? value : null
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
