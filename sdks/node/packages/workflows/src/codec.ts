// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createCipheriv, createDecipheriv, randomBytes } from 'node:crypto';
import type { JournalEntry, JsonValue, Outcome, StepWriteRequest } from './protocol.js';

/** App-owned encryption. Keys never travel to Temps. All participants use the same codec. */
export interface PayloadCodec {
  encode(value: JsonValue): Promise<JsonValue>;
  decode(value: JsonValue): Promise<JsonValue>;
}

export class PayloadCodecError extends Error {
  constructor() {
    super('Workflow payload codec failed: check the app key, key id and payload format');
    this.name = 'PayloadCodecError';
  }
}

/** AES-256-GCM with a key ring: keep old keys while their runs/events can still be replayed. */
export function aesGcmCodec(options: { activeKeyId: string; keys: Record<string, Uint8Array> }): PayloadCodec {
  const keys = new Map(Object.entries(options.keys).map(([id, key]) => [id, Buffer.from(key)]));
  if (!/^[a-zA-Z0-9_-]{1,64}$/.test(options.activeKeyId) || !keys.has(options.activeKeyId) ||
      [...keys.values()].some((key) => key.length !== 32)) throw new PayloadCodecError();
  const aad = (id: string) => Buffer.from(`temps-workflows/app-codec/v1:${id}`);
  return {
    async encode(value) {
      const id = options.activeKeyId;
      const key = keys.get(id);
      if (!key) throw new PayloadCodecError();
      const nonce = randomBytes(12);
      const cipher = createCipheriv('aes-256-gcm', key, nonce);
      cipher.setAAD(aad(id));
      const data = Buffer.concat([cipher.update(JSON.stringify(value), 'utf8'), cipher.final()]);
      return { codec: 'temps-aes-gcm-v1', key_id: id, nonce: nonce.toString('base64'),
        data: data.toString('base64'), tag: cipher.getAuthTag().toString('base64') };
    },
    async decode(value) {
      try {
        if (!value || typeof value !== 'object' || Array.isArray(value) || value.codec !== 'temps-aes-gcm-v1' ||
            typeof value.key_id !== 'string' || typeof value.nonce !== 'string' ||
            typeof value.data !== 'string' || typeof value.tag !== 'string') throw new PayloadCodecError();
        const key = keys.get(value.key_id);
        const nonce = Buffer.from(value.nonce, 'base64');
        const tag = Buffer.from(value.tag, 'base64');
        if (!key || nonce.length !== 12 || tag.length !== 16) throw new PayloadCodecError();
        const decipher = createDecipheriv('aes-256-gcm', key, nonce);
        decipher.setAAD(aad(value.key_id));
        decipher.setAuthTag(tag);
        return JSON.parse(Buffer.concat([decipher.update(Buffer.from(value.data, 'base64')), decipher.final()]).toString('utf8')) as JsonValue;
      } catch {
        // Never forward crypto errors, custom codec errors, keys or payload contents.
        throw new PayloadCodecError();
      }
    },
  };
}

export async function encode(codec: PayloadCodec, value: JsonValue): Promise<JsonValue> {
  try { return await codec.encode(value); } catch { throw new PayloadCodecError(); }
}
export async function decode(codec: PayloadCodec, value: JsonValue): Promise<JsonValue> {
  try { return await codec.decode(value); } catch { throw new PayloadCodecError(); }
}
const ERROR_PREFIX = 'temps-codec-v1:';
export async function encodeError(codec: PayloadCodec, error: string): Promise<string> {
  // Leave room for JSON escaping and base64 below the server's 64 KiB error cap.
  let end = error.length;
  while (Buffer.byteLength(JSON.stringify(error.slice(0, end)), 'utf8') > 24 * 1024) end = Math.floor(end / 2);
  const message = end < error.length ? `${error.slice(0, end)}…` : error;
  const encoded = ERROR_PREFIX + JSON.stringify(await encode(codec, message));
  // Custom codecs must also fit: truncating ciphertext makes it undecryptable.
  if (Buffer.byteLength(JSON.stringify(encoded), 'utf8') > 60 * 1024) throw new PayloadCodecError();
  return encoded;
}
export async function decodeError(codec: PayloadCodec, error: string): Promise<string> {
  // Server-generated errors are metadata, not app payloads.
  if (!error.startsWith(ERROR_PREFIX)) return error;
  try {
    const value = await decode(codec, JSON.parse(error.slice(ERROR_PREFIX.length)) as JsonValue);
    if (typeof value !== 'string') throw new PayloadCodecError();
    return value;
  } catch { throw new PayloadCodecError(); }
}

export async function encodeWrite(codec: PayloadCodec, write: StepWriteRequest): Promise<StepWriteRequest> {
  return { ...write,
    result: write.result != null && ['step', 'now', 'random'].includes(write.kind) ? await encode(codec, write.result) : write.result,
    error: write.error != null ? await encodeError(codec, write.error) : write.error,
    event: write.event ? { ...write.event, payload: await encode(codec, write.event.payload ?? null) } : write.event,
    child: write.child ? { ...write.child, input: await encode(codec, write.child.input ?? null) } : write.child,
  };
}

export async function decodeEntry(codec: PayloadCodec, entry: JournalEntry): Promise<JournalEntry> {
  let result = entry.result;
  if (result != null) {
    if (['step', 'now', 'random'].includes(entry.kind)) result = await decode(codec, result);
    else if (entry.kind === 'wait' && typeof result === 'object' && !Array.isArray(result) && result.received) {
      result = { ...result, payload: await decode(codec, result.payload ?? null) };
    } else if (entry.kind === 'child' && entry.status === 'completed' && typeof result === 'object' && !Array.isArray(result)) {
      result = { ...result,
        output: result.output != null ? await decode(codec, result.output) : null,
        error: typeof result.error === 'string' ? await decodeError(codec, result.error) : null };
    }
  }
  return { ...entry, result, error: entry.error != null ? await decodeError(codec, entry.error) : entry.error };
}

export async function encodeOutcome(codec: PayloadCodec, outcome: Outcome): Promise<Outcome> {
  if (outcome.type === 'completed') return { ...outcome, output: await encode(codec, outcome.output ?? null) };
  if (outcome.type === 'failed') return { ...outcome, error: await encodeError(codec, outcome.error) };
  return outcome;
}
