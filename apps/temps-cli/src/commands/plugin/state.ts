// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { open, rename, unlink, lstat } from "node:fs/promises";
import { constants } from "node:fs";
import { dirname } from "node:path";
import { PluginPublishError } from "./model.js";

export const object = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);
export const uuid = (v: unknown): v is string =>
  typeof v === "string" &&
  /^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(v);
export const timestamp = (v: unknown): v is string =>
  typeof v === "string" &&
  /^\d{4}-\d\d-\d\dT.*(Z|[+-]\d\d:\d\d)$/.test(v) &&
  Number.isFinite(Date.parse(v));

// Never truncate the recoverable copy. Sync content before rename, then sync
// the directory entry before allowing a remote side effect.
// Bun handles file contents. Keep Node's Bun-compatible filesystem primitives
// for O_EXCL/O_NOFOLLOW, atomic rename and fsync: Bun.write alone cannot provide
// these crash-recovery and symlink-safety guarantees.
export async function saveAtomic(path: string, value: unknown) {
  const temp = `${path}.${crypto.randomUUID()}.tmp`;
  let created = false;
  try {
    const current = await lstat(path).catch((error: NodeJS.ErrnoException) => {
      if (error.code === "ENOENT") return null;
      throw error;
    });
    if (current && (!current.isFile() || current.isSymbolicLink()))
      throw new Error("Unsafe state destination");
    const file = await open(
      temp,
      constants.O_WRONLY |
        constants.O_CREAT |
        constants.O_EXCL |
        constants.O_NOFOLLOW,
      0o600,
    );
    created = true;
    try {
      await Bun.write(Bun.file(file.fd), JSON.stringify(value, null, 2) + "\n");
      await file.sync();
    } finally {
      await file.close();
    }
    await rename(temp, path);
    const directory = await open(dirname(path), constants.O_RDONLY);
    try {
      await directory.sync();
    } finally {
      await directory.close();
    }
  } catch {
    throw new PluginPublishError(
      `Could not durably save ${path}. No further publication was attempted; preserve this directory and retry.`,
    );
  } finally {
    if (created) await unlink(temp).catch(() => undefined);
  }
}

export async function readState(path: string): Promise<unknown | undefined> {
  try {
    const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      return await Bun.file(file.fd).json();
    } finally {
      await file.close();
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw new PluginPublishError(
      `Could not read valid publication state at ${path}. Preserve it for recovery; do not create another release.`,
    );
  }
}
