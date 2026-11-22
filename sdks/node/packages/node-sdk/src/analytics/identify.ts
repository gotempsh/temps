// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Identify a visitor from the app's backend (ADR 109 D4).
 *
 * ```ts
 * import { analytics } from '@temps-sdk/node-sdk';
 *
 * // In the signup handler, with the incoming request:
 * await analytics.identify(request, { userId: user.id, traits: { plan: 'pro' } });
 * ```
 *
 * Zero configuration on Temps: the visitor is the request's
 * `_temps_visitor_id` cookie (set by the Temps proxy), and `TEMPS_API_URL`
 * and `TEMPS_API_TOKEN` are injected into every deployment. The token pins
 * the project. An identity set here is marked as vouched for by the backend:
 * a later `identify()` from the browser cannot replace it. Never throws;
 * the result says what happened.
 */

export type IdentifyTraitValue =
  | string
  | number
  | boolean
  | null
  | IdentifyTraitValue[]
  | { [key: string]: IdentifyTraitValue };

/** Anything carrying request headers: a Fetch `Request`, or Node's `IncomingMessage`. */
export type RequestLike =
  | { headers: { get(name: string): string | null } }
  | { headers: Record<string, string | string[] | undefined> };

export interface IdentifyOptions {
  /** The app's own id for the signed-in person. At most 256 characters. */
  userId: string;
  /** Merged into the visitor's attributes; `null` removes a key. 32 keys, 8 KB. */
  traits?: Record<string, IdentifyTraitValue>;
  /**
   * The visitor, when the request carries no `_temps_visitor_id` cookie,
   * sent as given. A deployment token only accepts the sealed reference:
   * `enc_` followed by the cookie's value (what `visitorIdFromRequest`
   * returns).
   */
  visitorId?: string;
  /**
   * Needed only with a credential that is not scoped to one project (a
   * personal API key). Deployment tokens carry their project.
   */
  projectId?: number;
  /** Defaults to `TEMPS_API_URL`. */
  apiUrl?: string;
  /** Defaults to `TEMPS_API_TOKEN`. */
  apiToken?: string;
  /** Request timeout. Defaults to 5 seconds. */
  timeoutMs?: number;
}

export interface IdentifyResult {
  /** Whether the visitor now carries this identity. */
  applied: boolean;
  /**
   * The server's outcome (`updated`, `unchanged`, `visitor_not_found`,
   * `cookieless_project`, `server_identity_kept`, …) or a local one when
   * nothing was sent: `no_visitor`, `not_configured`, `invalid_user_id`,
   * `network_error`, `http_<status>`.
   */
  outcome: string;
  message: string;
}

export const VISITOR_COOKIE = '_temps_visitor_id';
/** Marks a visitor reference as the proxy's sealed cookie value. */
const SEALED_PREFIX = 'enc_';
const DEFAULT_TIMEOUT_MS = 5_000;

function envVar(name: string): string | undefined {
  const global = globalThis as { process?: { env?: Record<string, string | undefined> } };
  return global.process?.env?.[name];
}

function header(request: RequestLike, name: string): string | undefined {
  const headers = request.headers as unknown;
  if (headers && typeof (headers as { get?: unknown }).get === 'function') {
    return (headers as { get(name: string): string | null }).get(name) ?? undefined;
  }
  const value = (headers as Record<string, string | string[] | undefined>)[name.toLowerCase()];
  return Array.isArray(value) ? value.join('; ') : value;
}

/**
 * The API reference of the visitor named by the request's
 * `_temps_visitor_id` cookie: `enc_` and the sealed value the proxy stores
 * there (it stores no prefix). `undefined` without the cookie.
 */
export function visitorIdFromRequest(request: RequestLike): string | undefined {
  const sealed = visitorCookie(request);
  return sealed ? `${SEALED_PREFIX}${sealed}` : undefined;
}

function visitorCookie(request: RequestLike): string | undefined {
  const cookies = header(request, 'cookie');
  if (!cookies) return undefined;
  for (const part of cookies.split(';')) {
    const separator = part.indexOf('=');
    if (separator === -1) continue;
    if (part.slice(0, separator).trim() !== VISITOR_COOKIE) continue;
    const value = part.slice(separator + 1).trim();
    if (!value) return undefined;
    try {
      return decodeURIComponent(value);
    } catch {
      return value;
    }
  }
  return undefined;
}

export async function identify(
  request: RequestLike | null,
  options: IdentifyOptions,
): Promise<IdentifyResult> {
  const { userId, traits, projectId } = options;
  if (typeof userId !== 'string' || userId.trim() === '') {
    return { applied: false, outcome: 'invalid_user_id', message: 'identify needs a non-empty userId.' };
  }
  const visitorId = options.visitorId ?? (request ? visitorIdFromRequest(request) : undefined);
  if (!visitorId) {
    return {
      applied: false,
      outcome: 'no_visitor',
      message: `The request has no ${VISITOR_COOKIE} cookie and no visitorId was given.`,
    };
  }
  const apiUrl = (options.apiUrl ?? envVar('TEMPS_API_URL') ?? '').replace(/\/+$/, '');
  const apiToken = options.apiToken ?? envVar('TEMPS_API_TOKEN') ?? '';
  if (!apiUrl || !apiToken) {
    return {
      applied: false,
      outcome: 'not_configured',
      message:
        'TEMPS_API_URL and TEMPS_API_TOKEN are required (both are injected automatically for apps deployed on Temps).',
    };
  }
  const visitorPath = `analytics/visitors/${encodeURIComponent(visitorId)}/identify`;
  const url =
    projectId === undefined ? `${apiUrl}/${visitorPath}` : `${apiUrl}/projects/${projectId}/${visitorPath}`;

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), options.timeoutMs ?? DEFAULT_TIMEOUT_MS);
  try {
    const response = await fetch(url, {
      method: 'PUT',
      headers: { Authorization: `Bearer ${apiToken}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ user_id: userId, traits }),
      signal: controller.signal,
    });
    const body = (await response.json().catch(() => null)) as
      | (Partial<IdentifyResult> & { detail?: string })
      | null;
    if (!response.ok) {
      return {
        applied: false,
        outcome: `http_${response.status}`,
        message: body?.detail ?? response.statusText,
      };
    }
    return {
      applied: Boolean(body?.applied),
      outcome: typeof body?.outcome === 'string' ? body.outcome : 'unknown',
      message: typeof body?.message === 'string' ? body.message : '',
    };
  } catch (error) {
    return { applied: false, outcome: 'network_error', message: String(error) };
  } finally {
    clearTimeout(timer);
  }
}
