// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The executor's HTTP surface: `GET /.well-known/temps/workflows/manifest`
 * and `POST /.well-known/temps/workflows/invoke`, as a Fetch API handler
 * (`(Request) => Promise<Response>`) that the adapters mount in Node's http
 * server, Express, Next.js or Hono.
 *
 * Every request must carry an invocation token signed by this Temps
 * instance for this app's own project and environment; the handler verifies
 * it before reading the body or running any workflow code. The key, and the
 * environment it binds tokens to, are read with the environment's workflow
 * credential (`TEMPS_WORKFLOWS_TOKEN`).
 */

import type { KeyObject } from 'node:crypto';

import {
  INVOKE_PATH,
  MANIFEST_PATH,
  PROTOCOL_HEADER,
  PROTOCOL_VERSION,
  type InvocationKey,
  type InvokeRequest,
  type Manifest,
} from './protocol.js';
import { runInvocation } from './replay.js';
import { Telemetry, type TelemetryOptions } from './telemetry.js';
import {
  InvalidTokenError,
  bearerToken,
  checkInvokeClaims,
  publicKeyFrom,
  verifyToken,
} from './token.js';
import { ExecutorTransport, type FetchLike } from './transport.js';
import type { WorkflowDefinition } from './workflow.js';

export const SDK_NAME = '@temps-sdk/workflows/0.1.0';

/** Longest invoke body accepted (the journal travels in it). */
export const MAX_INVOKE_BODY = 64 * 1024 * 1024;
/** Refetch the public key at most this often after a failed verification. */
const KEY_REFRESH_MS = 60_000;

export interface WorkflowHandlerOptions extends TelemetryOptions {
  workflows: WorkflowDefinition[];
  /** Temps' API root; default `process.env.TEMPS_API_URL`. */
  apiUrl?: string;
  /**
   * The environment's workflow credential, used to read the invocation key
   * and the project and environment it accepts tokens for; default
   * `process.env.TEMPS_WORKFLOWS_TOKEN`.
   */
  token?: string;
  /** Pin the invocation key (and its environment) instead of fetching it (tests, air-gapped setups). */
  invocationKey?: InvocationKey;
  fetch?: FetchLike;
  /** Heartbeat interval override, for tests. */
  heartbeatMs?: number;
}

export type FetchHandler = (request: Request) => Promise<Response>;

function problem(status: number, title: string, detail: string): Response {
  return new Response(JSON.stringify({ type: 'about:blank', status, title, detail }), {
    status,
    headers: { 'content-type': 'application/problem+json', [PROTOCOL_HEADER]: String(PROTOCOL_VERSION) },
  });
}

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json', [PROTOCOL_HEADER]: String(PROTOCOL_VERSION) },
  });
}

interface BoundKey {
  key: KeyObject;
  projectId: number;
  environmentId: number;
}

function bind(key: InvocationKey): BoundKey {
  return { key: publicKeyFrom(key), projectId: key.project_id, environmentId: key.environment_id };
}

/** The body, or `undefined` once it passes `limit` bytes (the rest is not read). */
async function readBounded(request: Request, limit: number): Promise<Uint8Array | undefined> {
  if (!request.body) return new Uint8Array();
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > limit) {
      await reader.cancel();
      return undefined;
    }
    chunks.push(value);
  }
  const body = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return body;
}

class KeyCache {
  private key?: Promise<BoundKey>;
  private refreshedAt = 0;
  private readonly pinned: boolean;

  constructor(
    private readonly transport: ExecutorTransport,
    private readonly credential: string | undefined,
    pinned?: InvocationKey,
  ) {
    this.pinned = pinned !== undefined;
    if (pinned) this.key = Promise.resolve(bind(pinned));
  }

  get(): Promise<BoundKey> {
    if (!this.key) {
      const credential = this.credential;
      if (!credential) {
        return Promise.reject(
          new Error(
            'TEMPS_WORKFLOWS_TOKEN is not set: the handler reads the invocation key with the environment\'s workflow credential. Issue one in the console (environment → Workflows) and set it in the app.',
          ),
        );
      }
      this.refreshedAt = Date.now();
      this.key = this.transport.invocationKey(credential).then(bind);
      this.key.catch(() => {
        this.key = undefined;
      });
    }
    return this.key;
  }

  /** After a failed verification: the key may have rotated. */
  refresh(): boolean {
    if (this.pinned || Date.now() - this.refreshedAt < KEY_REFRESH_MS) return false;
    this.key = undefined;
    return true;
  }
}

/**
 * Create the workflow handler.
 *
 * ```ts
 * const handler = createWorkflowHandler({ workflows: [sendInvoice] });
 * // Node http / Express:  app.use(toNodeListener(handler))
 * // Next.js App Router:    app/.well-known/temps/workflows/[action]/route.ts
 * //                        export const { GET, POST } = nextRouteHandlers(handler)
 * // Hono:                  app.all('/.well-known/temps/workflows/*', (c) => handler(c.req.raw))
 * ```
 */
export function createWorkflowHandler(options: WorkflowHandlerOptions): FetchHandler {
  const apiUrl = options.apiUrl ?? process.env.TEMPS_API_URL;
  if (!apiUrl) {
    throw new Error(
      'createWorkflowHandler: TEMPS_API_URL is not set. Temps sets it in every deployment; pass apiUrl when running elsewhere.',
    );
  }
  const workflows = new Map<string, WorkflowDefinition>();
  for (const definition of options.workflows) {
    if (workflows.has(definition.name)) {
      throw new Error(`createWorkflowHandler: workflow '${definition.name}' is defined twice`);
    }
    workflows.set(definition.name, definition);
  }
  const transport = new ExecutorTransport({ apiUrl, fetch: options.fetch });
  const keys = new KeyCache(transport, options.token ?? process.env.TEMPS_WORKFLOWS_TOKEN, options.invocationKey);
  const telemetry = new Telemetry(options);
  const manifest: Manifest = {
    protocol_version: PROTOCOL_VERSION,
    sdk: SDK_NAME,
    workflows: [...workflows.values()].map((definition) => ({
      name: definition.name,
      version: definition.options.version ?? null,
      concurrency: definition.options.concurrency ?? null,
    })),
  };

  async function verifiedWith(token: string) {
    const bound = await keys.get();
    const claims = verifyToken(token, bound.key);
    // The key is the instance's: a token Temps signed for another
    // environment (whose executor could forward it here) is valid too.
    if (claims.pid !== bound.projectId || claims.eid !== bound.environmentId) {
      throw new InvalidTokenError('token is for another project or environment');
    }
    return claims;
  }

  async function verified(request: Request) {
    const token = bearerToken(request.headers.get('authorization'));
    if (!token) throw new InvalidTokenError('missing bearer token');
    try {
      return { token, claims: await verifiedWith(token) };
    } catch (error) {
      if (error instanceof InvalidTokenError && error.reason === 'bad signature' && keys.refresh()) {
        return { token, claims: await verifiedWith(token) };
      }
      throw error;
    }
  }

  async function handleManifest(request: Request): Promise<Response> {
    const { claims } = await verified(request);
    if (claims.typ !== 'manifest') throw new InvalidTokenError('token is not for a manifest read');
    return json(200, manifest);
  }

  async function handleInvoke(request: Request): Promise<Response> {
    const length = Number(request.headers.get('content-length') ?? '0');
    if (length > MAX_INVOKE_BODY) return problem(413, 'Payload Too Large', `invoke body over ${MAX_INVOKE_BODY} bytes`);
    // The token first: nobody but Temps gets this handler to read a body.
    const { token, claims } = await verified(request);
    const body = await readBounded(request, MAX_INVOKE_BODY);
    if (!body) return problem(413, 'Payload Too Large', `invoke body over ${MAX_INVOKE_BODY} bytes`);
    let invoke: InvokeRequest;
    try {
      invoke = JSON.parse(new TextDecoder().decode(body)) as InvokeRequest;
    } catch {
      return problem(400, 'Invalid Invoke Request', 'the body is not JSON');
    }
    checkInvokeClaims(claims, body, invoke);
    if (invoke.protocol_version !== PROTOCOL_VERSION) {
      return problem(
        400,
        'Unsupported Protocol Version',
        `this SDK speaks protocol ${PROTOCOL_VERSION}, the request is version ${invoke.protocol_version}`,
      );
    }
    const definition = workflows.get(invoke.workflow);
    if (!definition) {
      return json(200, {
        outcome: {
          type: 'failed',
          error: `Workflow '${invoke.workflow}' is not defined in this deployment (it defines: ${[...workflows.keys()].join(', ') || 'none'})`,
        },
      });
    }
    const result = await runInvocation(definition, invoke, {
      transport,
      telemetry,
      token,
      heartbeatMs: options.heartbeatMs,
    });
    if (result.type === 'stale') return new Response(null, { status: 202 });
    if (result.type === 'unavailable') return problem(503, 'Temps Unavailable', result.reason);
    return json(200, { outcome: result.outcome });
  }

  return async (request: Request): Promise<Response> => {
    const path = new URL(request.url).pathname;
    try {
      if (path.endsWith(MANIFEST_PATH)) {
        if (request.method !== 'GET') return problem(405, 'Method Not Allowed', 'use GET');
        return await handleManifest(request);
      }
      if (path.endsWith(INVOKE_PATH)) {
        if (request.method !== 'POST') return problem(405, 'Method Not Allowed', 'use POST');
        return await handleInvoke(request);
      }
      return problem(404, 'Not Found', `no workflow endpoint at ${path}`);
    } catch (error) {
      if (error instanceof InvalidTokenError) return problem(401, 'Invalid Invocation Token', error.reason);
      return problem(500, 'Workflow Handler Error', error instanceof Error ? error.message : String(error));
    }
  };
}
