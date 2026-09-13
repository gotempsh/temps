// SPDX-License-Identifier: MIT OR Apache-2.0
import { spawn } from "node:child_process";
import {
  chmod,
  lstat,
  mkdir,
  readFile,
  realpath,
  readdir,
  open,
  unlink,
} from "node:fs/promises";
import { constants } from "node:fs";
import { resolve, join, sep } from "node:path";
import { createHash } from "node:crypto";
import { getCloudUrl, requireCloudAuth } from "../../lib/cloud-client.js";
import {
  TARGETS,
  parseConfig,
  releaseMetadata,
  platformManifest,
  packageName,
  PluginPublishError,
  type PluginConfig,
} from "./model.js";

export async function command(argv: string[], cwd: string) {
  const executable = argv[0];
  if (!executable) throw new PluginPublishError("Missing command executable.");
  await new Promise<void>((ok, fail) => {
    const child = spawn(executable, argv.slice(1), {
      cwd,
      stdio: "inherit",
      shell: false,
    });
    child.on("error", () =>
      fail(
        new PluginPublishError(
          `Could not start ${argv[0]}. Install it and retry.`,
        ),
      ),
    );
    child.on("exit", (code) =>
      code === 0
        ? ok()
        : fail(
            new PluginPublishError(
              `${argv[0]} failed (${code}); no further packages were published. Retry plugin publish to resume.`,
            ),
          ),
    );
  });
}
export async function loadConfig(cwd: string) {
  let value: unknown;
  try {
    value = JSON.parse(await readFile(join(cwd, "package.json"), "utf8"));
  } catch {
    throw new PluginPublishError(
      `Could not read valid package.json in ${cwd}. Run plugin init in a new directory or correct the file.`,
    );
  }
  return parseConfig(value);
}
async function directory(path: string) {
  await mkdir(path, { recursive: true, mode: 0o700 });
  if (
    !(await lstat(path)).isDirectory() ||
    (await lstat(path)).isSymbolicLink()
  )
    throw new PluginPublishError(`Refusing non-directory or symlink: ${path}`);
}
async function save(path: string, value: unknown) {
  // No symlink following, including when resuming a partially published release.
  const file = await open(
    path,
    constants.O_WRONLY |
      constants.O_CREAT |
      constants.O_TRUNC |
      constants.O_NOFOLLOW,
    0o600,
  );
  try {
    await file.writeFile(JSON.stringify(value, null, 2) + "\n");
  } finally {
    await file.close();
  }
}
export type State = {
  digest: string;
  id: string;
  expiresAt: string;
  challenges: Array<{
    id: string;
    name: string;
    platform: keyof typeof TARGETS;
    code: string;
  }>;
};
export const configDigest = (c: PluginConfig) =>
  createHash("sha256")
    .update(JSON.stringify(releaseMetadata(c)))
    .digest("hex");
export function validateState(state: State, c: PluginConfig) {
  if (
    state.digest !== configDigest(c) ||
    !/^[0-9a-f-]{36}$/.test(state.id) ||
    !Array.isArray(state.challenges) ||
    state.challenges.length !== c.temps.platforms.length ||
    state.challenges.some(
      (p, i) =>
        p.platform !== c.temps.platforms[i] ||
        p.name !== packageName(c, p.platform) ||
        !/^[0-9a-f-]{36}$/.test(p.id) ||
        !/^[A-Za-z0-9_-]{43}$/.test(p.code),
    )
  )
    throw new PluginPublishError(
      "Saved release does not match package.json. Restore its metadata or choose a new version.",
    );
}
async function publisher(operation: unknown): Promise<unknown> {
  const origin = new URL(getCloudUrl());
  if (
    origin.protocol !== "https:" ||
    origin.username ||
    origin.password ||
    origin.pathname !== "/" ||
    origin.search ||
    origin.hash
  )
    throw new PluginPublishError(
      "Publishing requires an HTTPS Temps account endpoint.",
    );
  const token = await requireCloudAuth();
  const response = await fetch(new URL("/api/plugin-publisher", origin), {
    method: "POST",
    redirect: "error",
    signal: AbortSignal.timeout(90000),
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(operation),
  });
  const data = (await response.json()) as { error?: string };
  if (!response.ok)
    throw new PluginPublishError(
      typeof data.error === "string"
        ? data.error
        : `Publisher returned ${response.status}.`,
    );
  return data;
}
export async function buildPlugin(
  cwd: string,
  c: PluginConfig,
  state?: State,
  run = command,
) {
  const root = await realpath(cwd);
  const source = await realpath(join(root, c.temps.entrypoint));
  if (!source.startsWith(root + sep))
    throw new PluginPublishError("Plugin source must stay inside the project.");
  const base = join(root, ".temps-plugin");
  await directory(base);
  const out = join(base, c.version);
  await directory(out);
  for (const platform of c.temps.platforms) {
    const path = join(out, platform);
    await directory(path);
    const binary = join(path, "plugin");
    for (const file of [binary, join(path, "package.json")]) {
      const stat = await lstat(file).catch(() => null);
      if (stat && (!stat.isFile() || stat.isSymbolicLink()))
        throw new PluginPublishError(`Refusing unsafe output: ${file}`);
    }
    await run(
      [
        "bun",
        "build",
        source,
        "--compile",
        `--target=${TARGETS[platform].target}`,
        "--outfile",
        binary,
      ],
      root,
    );
    await chmod(binary, 0o755);
    await save(
      join(path, "package.json"),
      platformManifest(
        c,
        platform,
        state?.challenges.find((p) => p.platform === platform)?.code,
      ),
    );
  }
  return out;
}
async function npmExists(name: string, version: string) {
  const response = await fetch(
    `https://registry.npmjs.org/${encodeURIComponent(name)}/${encodeURIComponent(version)}`,
    { method: "HEAD", redirect: "error", signal: AbortSignal.timeout(15000) },
  );
  if (response.status === 404) return false;
  if (!response.ok)
    throw new PluginPublishError(
      `npm metadata unavailable (${response.status}); publishing stopped.`,
    );
  return true;
}
export type PublishDependencies = {
  publisher: typeof publisher;
  npmExists: typeof npmExists;
  command: typeof command;
  buildPlugin: typeof buildPlugin;
};
export async function publishPlugin(
  cwd: string,
  c: PluginConfig,
  deps: PublishDependencies = { publisher, npmExists, command, buildPlugin },
) {
  const base = join(cwd, ".temps-plugin");
  await directory(base);
  const lock = join(base, "publish.lock");
  const lease = await open(lock, "wx", 0o600).catch(() => {
    throw new PluginPublishError(
      "Another publication is running, or a previous process stopped. Remove .temps-plugin/publish.lock only after confirming no publication is running.",
    );
  });
  try {
    await publishLocked(cwd, c, deps);
  } finally {
    await lease.close();
    await unlink(lock);
  }
}
async function publishLocked(
  cwd: string,
  c: PluginConfig,
  { publisher, npmExists, command, buildPlugin }: PublishDependencies,
) {
  const base = join(cwd, ".temps-plugin");
  const out = join(base, c.version);
  await directory(out);
  const stateFile = join(out, "release.json");
  const stat = await lstat(stateFile).catch(() => null);
  if (stat && (!stat.isFile() || stat.isSymbolicLink()))
    throw new PluginPublishError("Unsafe release state file.");
  let state: State;
  if (stat) {
    state = JSON.parse(await readFile(stateFile, "utf8")) as State;
    validateState(state, c);
  } else {
    for (const platform of c.temps.platforms)
      if (await npmExists(packageName(c, platform), c.version))
        throw new PluginPublishError(
          `${packageName(c, platform)}@${c.version} already exists. Choose a new unpublished version.`,
        );
    // Compile before creating the expiring challenge; no npm publication yet.
    await buildPlugin(cwd, c);
    const created = (await publisher({
      operation: "create",
      metadata: releaseMetadata(c),
    })) as Omit<State, "digest">;
    state = { ...created, digest: configDigest(c) };
    validateState(state, c);
    await save(stateFile, state);
  }
  const status = (await publisher({ operation: "status", id: state.id })) as {
    status: string;
    packages: Array<{ id: string; verifiedAt: string | null }>;
  };
  if (status.status === "pending" || status.status === "approved") {
    console.log(
      `Release already ${status.status}. See https://temps.sh/dashboard/plugins`,
    );
    return;
  }
  if (status.status === "rejected")
    throw new PluginPublishError(
      "Release rejected. Review the notes in My plugins and create a new version.",
    );
  for (const challenge of state.challenges) {
    if (status.packages.some((p) => p.id === challenge.id && p.verifiedAt))
      continue;
    if (Date.parse(state.expiresAt) <= Date.now())
      throw new PluginPublishError(
        "Verification codes expired. Use a new version and publish again.",
      );
    if (!(await npmExists(challenge.name, c.version))) {
      const dir = join(out, challenge.platform);
      const directoryInfo = await lstat(dir);
      if (!directoryInfo.isDirectory() || directoryInfo.isSymbolicLink())
        throw new PluginPublishError("Unsafe platform output directory.");
      if (
        (await readdir(dir)).some(
          (name) => name !== "plugin" && name !== "package.json",
        )
      )
        throw new PluginPublishError(
          `Unexpected files in ${dir}. Only plugin and package.json may be published.`,
        );
      const binary = await lstat(join(dir, "plugin")).catch(() => null);
      if (!binary?.isFile() || binary.isSymbolicLink())
        throw new PluginPublishError(
          "Build output missing. Run plugin build --all, then plugin publish again.",
        );
      const manifest = join(dir, "package.json");
      if ((await lstat(manifest)).isSymbolicLink())
        throw new PluginPublishError("Unsafe package manifest.");
      await save(
        manifest,
        platformManifest(c, challenge.platform, challenge.code),
      );
      // npm handles the user's npm login/2FA; no shell, lifecycle scripts or root files.
      await command(
        [
          "npm",
          "publish",
          "--access",
          "public",
          "--ignore-scripts",
          "--registry=https://registry.npmjs.org",
        ],
        dir,
      );
    }
    // Already-published packages are never trusted on name alone: server verifies
    // the nonce and archive before allowing submission, also on resumed runs.
    await publisher({ operation: "verify", id: challenge.id });
  }
  await publisher({ operation: "submit", id: state.id });
  console.log(
    "Submitted for review. Approval and protected registry signing remain separate. https://temps.sh/dashboard/plugins",
  );
}
