// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Invocation token verification.
 *
 * Temps signs every request it sends to the executor with Ed25519
 * (`twi1.<claims>.<signature>`). The handler verifies the token before any
 * workflow code runs: signature, expiry, purpose, and for an invocation the
 * SHA-256 of the exact body, so a forged or replayed-with-other-input request
 * is refused.
 */

import { createHash, createPublicKey, verify, type KeyObject } from 'node:crypto';

import { TOKEN_VERSION, type InvocationClaims, type InvocationKey } from './protocol.js';

const MAX_TOKEN_LENGTH = 4096;

export class InvalidTokenError extends Error {
  constructor(public readonly reason: string) {
    super(`Invalid invocation token: ${reason}`);
    this.name = 'InvalidTokenError';
  }
}

export function sha256Hex(bytes: Uint8Array | string): string {
  return createHash('sha256').update(bytes).digest('hex');
}

export function publicKeyFrom(key: InvocationKey): KeyObject {
  if (key.algorithm !== 'ed25519') {
    throw new InvalidTokenError(`unsupported key algorithm '${key.algorithm}'`);
  }
  return createPublicKey({ key: { kty: 'OKP', crv: 'Ed25519', x: key.public_key }, format: 'jwk' });
}

export function bearerToken(header: string | null | undefined): string | undefined {
  if (!header) return undefined;
  const match = /^Bearer\s+(\S+)$/i.exec(header.trim());
  return match?.[1];
}

/** Verify the signature and expiry of `token` against `key`; returns its claims. */
export function verifyToken(
  token: string,
  key: KeyObject,
  nowSeconds: number = Math.floor(Date.now() / 1000),
): InvocationClaims {
  if (token.length > MAX_TOKEN_LENGTH) throw new InvalidTokenError('token too long');
  const parts = token.split('.');
  if (parts.length !== 3) throw new InvalidTokenError('malformed token');
  const [version, payload, signature] = parts as [string, string, string];
  if (version !== TOKEN_VERSION) throw new InvalidTokenError('unknown token version');
  let valid = false;
  try {
    valid = verify(
      null,
      Buffer.from(`${version}.${payload}`, 'ascii'),
      key,
      Buffer.from(signature, 'base64url'),
    );
  } catch {
    valid = false;
  }
  if (!valid) throw new InvalidTokenError('bad signature');
  let claims: InvocationClaims;
  try {
    claims = JSON.parse(Buffer.from(payload, 'base64url').toString('utf8')) as InvocationClaims;
  } catch {
    throw new InvalidTokenError('malformed claims');
  }
  if (claims.v !== 1) throw new InvalidTokenError('unknown claims version');
  if (typeof claims.exp !== 'number' || claims.exp <= nowSeconds) {
    throw new InvalidTokenError('token expired');
  }
  return claims;
}

/** The checks specific to an invoke request, after {@link verifyToken}. */
export function checkInvokeClaims(
  claims: InvocationClaims,
  body: Uint8Array,
  parsed: { run_id: string; lease: number },
): void {
  if (claims.typ !== 'invoke') throw new InvalidTokenError('token is not for an invocation');
  if (claims.body !== sha256Hex(body)) {
    throw new InvalidTokenError('token was issued for another request body');
  }
  if (claims.run !== parsed.run_id || claims.lease !== parsed.lease) {
    throw new InvalidTokenError('token is for another run or lease');
  }
}
