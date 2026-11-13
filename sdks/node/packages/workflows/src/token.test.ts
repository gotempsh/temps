// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { generateKeyPairSync } from 'node:crypto';
import { describe, expect, it } from 'vitest';

import { FakeOrchestrator } from './testing/fake-orchestrator.js';
import { InvalidTokenError, bearerToken, checkInvokeClaims, publicKeyFrom, sha256Hex, verifyToken } from './token.js';

const temps = new FakeOrchestrator();
const key = publicKeyFrom(temps.invocationKey);
const now = Math.floor(Date.now() / 1000);
const claims = { v: 1, typ: 'invoke' as const, pid: 1, eid: 2, run: 'r1', lease: 3, exp: now + 60, body: sha256Hex('{}') };

function reason(fn: () => unknown): string {
  try {
    fn();
  } catch (error) {
    if (error instanceof InvalidTokenError) return error.reason;
    throw error;
  }
  return 'accepted';
}

describe('invocation tokens', () => {
  it('accepts a token Temps signed and returns its claims', () => {
    expect(verifyToken(temps.signToken(claims), key, now)).toEqual(claims);
  });

  it('refuses a token signed by another key', () => {
    const other = new FakeOrchestrator();
    expect(reason(() => verifyToken(other.signToken(claims), key, now))).toBe('bad signature');
  });

  it('refuses expired, malformed, versioned and oversized tokens', () => {
    expect(reason(() => verifyToken(temps.signToken({ ...claims, exp: now }), key, now))).toBe('token expired');
    expect(reason(() => verifyToken('twi1.only-two', key, now))).toBe('malformed token');
    const [, payload, signature] = temps.signToken(claims).split('.');
    expect(reason(() => verifyToken(`twi2.${payload}.${signature}`, key, now))).toBe('unknown token version');
    expect(reason(() => verifyToken('x'.repeat(5000), key, now))).toBe('token too long');
    expect(reason(() => verifyToken(temps.signToken({ ...claims, v: 2 }), key, now))).toBe('unknown claims version');
  });

  it('binds an invocation to its body, run and lease', () => {
    const body = new TextEncoder().encode('{}');
    expect(reason(() => checkInvokeClaims(claims, body, { run_id: 'r1', lease: 3 }))).toBe('accepted');
    expect(reason(() => checkInvokeClaims(claims, new TextEncoder().encode('{"x":1}'), { run_id: 'r1', lease: 3 }))).toBe(
      'token was issued for another request body',
    );
    expect(reason(() => checkInvokeClaims(claims, body, { run_id: 'r1', lease: 4 }))).toBe('token is for another run or lease');
    expect(reason(() => checkInvokeClaims({ ...claims, typ: 'manifest' }, body, { run_id: 'r1', lease: 3 }))).toBe(
      'token is not for an invocation',
    );
  });

  it('reads bearer headers and refuses a key of another algorithm', () => {
    expect(bearerToken('Bearer abc')).toBe('abc');
    expect(bearerToken('bearer   abc ')).toBe('abc');
    expect(bearerToken('Basic abc')).toBeUndefined();
    expect(bearerToken(null)).toBeUndefined();
    const { publicKey } = generateKeyPairSync('ed25519');
    const x = (publicKey.export({ format: 'jwk' }) as { x: string }).x;
    expect(() => publicKeyFrom({ algorithm: 'rsa', public_key: x, key_id: 'k' })).toThrow(/unsupported key algorithm/);
  });
});
