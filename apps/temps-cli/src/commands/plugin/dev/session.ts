// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import {
  mkdir,
  lstat,
  chmod,
  readFile,
  rename,
  writeFile,
} from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { DevError, MAX_BODY } from "./model.js";
export function sessionPaths(session: string, home = homedir()) {
  if (!/^[a-zA-Z0-9][a-zA-Z0-9_-]{0,39}$/.test(session))
    throw new DevError(
      "Session must be 1–40 letters, digits, underscores, or hyphens.",
    );
  const root = join(home, ".temps", "plugin-dev");
  const state = join(root, session);
  const hash = createHash("sha256").update(state).digest("hex").slice(0, 20);
  const runtimeRoot = `/tmp/temps-pdev-${process.getuid?.() ?? "user"}`;
  const runtime = join(runtimeRoot, hash);
  return {
    root,
    state,
    runtimeRoot,
    runtime,
    control: join(runtime, "c.sock"),
    socket: join(runtime, "p.sock"),
  };
}
export async function privateDirectory(path: string) {
  await mkdir(path, { recursive: true, mode: 0o700 });
  const st = await lstat(path);
  if (
    !st.isDirectory() ||
    st.isSymbolicLink() ||
    (process.getuid && st.uid !== process.getuid())
  )
    throw new DevError(
      `Unsafe session directory ${path}: expected a directory owned by the current user.`,
    );
  await chmod(path, 0o700);
}
export async function saveState(path: string, value: unknown) {
  const temp = `${path}.${crypto.randomUUID()}.tmp`;
  await writeFile(temp, JSON.stringify(value) + "\n", {
    flag: "wx",
    mode: 0o600,
  });
  await rename(temp, path);
}
export async function loadState(path: string): Promise<unknown | undefined> {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw new DevError(
      `Cannot read session state ${path}. Preserve the file and select a new --session, or repair the JSON.`,
    );
  }
}
export async function control(
  session: string,
  action: string,
  payload: unknown = {},
) {
  const { control: unix } = sessionPaths(session);
  try {
    const response = await fetch(`http://localhost/${action}`, {
      unix,
      method: "POST",
      body: JSON.stringify(payload),
      headers: { "content-type": "application/json" },
      signal: AbortSignal.timeout(30000),
    });
    const text = await response.text();
    if (Buffer.byteLength(text) > MAX_BODY)
      throw new DevError("Runner control response exceeds 1 MiB.");
    const result = JSON.parse(text);
    if (!response.ok)
      throw new DevError(
        result.error ?? `Runner returned HTTP ${response.status}.`,
      );
    return result;
  } catch (error) {
    if (error instanceof DevError) throw error;
    throw new DevError(
      `Cannot reach plugin dev session ${session}. Start it with bunx @temps-sdk/cli plugin dev <binary> --session ${session}.`,
    );
  }
}
