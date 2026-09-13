// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
export const TARGETS = {
  "linux-amd64-gnu": {
    target: "bun-linux-x64-baseline",
    os: "linux",
    cpu: "x64",
    libc: "glibc",
  },
  "linux-arm64-gnu": {
    target: "bun-linux-arm64",
    os: "linux",
    cpu: "arm64",
    libc: "glibc",
  },
  "linux-amd64-musl": {
    target: "bun-linux-x64-musl-baseline",
    os: "linux",
    cpu: "x64",
    libc: "musl",
  },
  "linux-arm64-musl": {
    target: "bun-linux-arm64-musl",
    os: "linux",
    cpu: "arm64",
    libc: "musl",
  },
  "darwin-amd64": { target: "bun-darwin-x64", os: "darwin", cpu: "x64" },
  "darwin-arm64": { target: "bun-darwin-arm64", os: "darwin", cpu: "arm64" },
} as const;
export type Platform = keyof typeof TARGETS;
export const CATEGORIES = [
  "Development",
  "Observability",
  "Data",
  "Security",
  "Integrations",
];
export const validName = (v: unknown): v is string =>
  typeof v === "string" &&
  /^@[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*$/.test(v) &&
  v.length <= 190;
export class PluginPublishError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "PluginPublishError";
  }
}
export type PluginConfig = {
  name: string;
  version: string;
  description: string;
  author: string;
  repository: string;
  temps: {
    name: string;
    title: string;
    category: string;
    entrypoint: string;
    platforms: Platform[];
  };
};
function record(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object" && !Array.isArray(v);
}
export function parseConfig(v: unknown): PluginConfig {
  if (!record(v) || !record(v.temps))
    throw new PluginPublishError(
      "package.json needs a temps manifest. Run temps plugin init in a new directory.",
    );
  const t = v.temps;
  if (
    !validName(v.name) ||
    typeof v.version !== "string" ||
    v.version.length > 100 ||
    !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/.test(
      v.version,
    )
  )
    throw new PluginPublishError(
      "Use a scoped npm name and exact release version in package.json.",
    );
  for (const key of ["description", "author", "repository"] as const)
    if (typeof v[key] !== "string" || !v[key].trim())
      throw new PluginPublishError(`package.json ${key} is required.`);
  if (
    (v.description as string).length > 4000 ||
    (v.author as string).length > 100
  )
    throw new PluginPublishError(
      "Description or author exceeds the registry limit.",
    );
  try {
    const url = new URL(v.repository as string);
    if (
      url.protocol !== "https:" ||
      url.username ||
      url.password ||
      url.href.length > 500
    )
      throw new Error();
  } catch {
    throw new PluginPublishError(
      "repository must be an HTTPS URL without credentials.",
    );
  }
  if (
    typeof t.name !== "string" ||
    !/^[a-z0-9][a-z0-9-]{0,63}$/.test(t.name) ||
    typeof t.title !== "string" ||
    !t.title.trim() ||
    t.title.length > 100 ||
    !CATEGORIES.includes(String(t.category))
  )
    throw new PluginPublishError(
      "Set a valid temps.name, title, and category in package.json.",
    );
  if (
    typeof t.entrypoint !== "string" ||
    !/^src\/[a-zA-Z0-9_./-]+\.tsx?$/.test(t.entrypoint) ||
    t.entrypoint.split("/").includes("..")
  )
    throw new PluginPublishError(
      "temps.entrypoint must be a TypeScript file within src/.",
    );
  if (
    !Array.isArray(t.platforms) ||
    !t.platforms.length ||
    t.platforms.length > 6 ||
    new Set(t.platforms).size !== t.platforms.length ||
    t.platforms.some((p) => !Object.hasOwn(TARGETS, String(p)))
  )
    throw new PluginPublishError(
      "temps.platforms must contain unique supported platform keys.",
    );
  return v as unknown as PluginConfig;
}
export const packageName = (config: PluginConfig, platform: Platform) =>
  `${config.name}-${platform.replace("amd64", "x64")}`;
export function releaseMetadata(c: PluginConfig) {
  return {
    name: c.temps.name,
    version: c.version,
    title: c.temps.title,
    category: c.temps.category,
    author: c.author,
    repository: c.repository,
    summary: c.description.slice(0, 200),
    description: c.description,
    packages: c.temps.platforms.map((platform) => ({
      platform,
      name: packageName(c, platform),
    })),
  };
}
export function platformManifest(
  c: PluginConfig,
  platform: Platform,
  code?: string,
) {
  const { target: _, ...native } = TARGETS[platform];
  return {
    name: packageName(c, platform),
    version: c.version,
    description: c.description,
    author: c.author,
    repository: c.repository,
    os: [native.os],
    cpu: [native.cpu],
    ...("libc" in native ? { libc: [native.libc] } : {}),
    files: ["plugin"],
    ...(code ? { temps: { verification: code } } : {}),
  };
}
