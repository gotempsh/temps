// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Mount the Fetch API handler in a framework.
 *
 * - Node `http` / Express / Connect: `toNodeListener(handler)`; requests
 *   outside `/.well-known/temps/workflows/` go to `next()` (or 404).
 * - Next.js App Router: `export const { GET, POST } = nextRouteHandlers(handler)`
 *   in `app/.well-known/temps/workflows/[action]/route.ts`.
 * - Hono: `app.all('/.well-known/temps/workflows/*', honoHandler(handler))`.
 */

import type { IncomingMessage, ServerResponse } from 'node:http';

import type { FetchHandler } from './handler.js';

export const WORKFLOWS_PREFIX = '/.well-known/temps/workflows/';

type Next = (error?: unknown) => void;

async function readBody(request: IncomingMessage): Promise<Uint8Array> {
  const chunks: Uint8Array[] = [];
  for await (const chunk of request) {
    chunks.push(typeof chunk === 'string' ? new TextEncoder().encode(chunk) : (chunk as Uint8Array));
  }
  return Buffer.concat(chunks);
}

/** A `(req, res, next?)` listener for Node's http server, Express or Connect. */
export function toNodeListener(handler: FetchHandler) {
  return async (request: IncomingMessage, response: ServerResponse, next?: Next): Promise<void> => {
    const path = (request.url ?? '/').split('?')[0] ?? '/';
    if (!path.startsWith(WORKFLOWS_PREFIX)) {
      if (next) return next();
      response.statusCode = 404;
      response.end();
      return;
    }
    try {
      const headers = new Headers();
      for (const [name, value] of Object.entries(request.headers)) {
        if (Array.isArray(value)) for (const item of value) headers.append(name, item);
        else if (value !== undefined) headers.set(name, value);
      }
      const method = request.method ?? 'GET';
      const body = method === 'GET' || method === 'HEAD' ? undefined : await readBody(request);
      const host = request.headers.host ?? 'localhost';
      const reply = await handler(
        new Request(`http://${host}${request.url ?? '/'}`, { method, headers, body: body as BodyInit | undefined }),
      );
      response.statusCode = reply.status;
      reply.headers.forEach((value, name) => response.setHeader(name, value));
      response.end(Buffer.from(await reply.arrayBuffer()));
    } catch (error) {
      if (next) return next(error);
      response.statusCode = 500;
      response.end();
    }
  };
}

/** `GET` and `POST` route handlers for the Next.js App Router. */
export function nextRouteHandlers(handler: FetchHandler): { GET: FetchHandler; POST: FetchHandler } {
  return { GET: handler, POST: handler };
}

/** A Hono handler: `app.all('/.well-known/temps/workflows/*', honoHandler(handler))`. */
export function honoHandler(handler: FetchHandler) {
  return (context: { req: { raw: Request } }): Promise<Response> => handler(context.req.raw);
}
