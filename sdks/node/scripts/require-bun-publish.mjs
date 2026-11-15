// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Refuses to publish a package whose manifest depends on a sibling through
// `workspace:*` unless `bun publish` is doing it. Bun swaps those ranges for
// the sibling's real version in the published tarball; `npm publish` copies
// them as they are, and the package then cannot be installed by anyone
// (`npm error code EUNSUPPORTEDPROTOCOL`). This happened once to
// @temps-sdk/react-analytics@0.0.8.
//
// Run from a package's `prepublishOnly`, so the working directory is the
// package being published.

import { readFileSync } from "node:fs";
import { join } from "node:path";

const manifestPath = join(process.cwd(), "package.json");
const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));

const fields = ["dependencies", "peerDependencies", "optionalDependencies"];
const workspaceRanges = fields.flatMap((field) =>
  Object.entries(manifest[field] ?? {})
    .filter(([, range]) => String(range).startsWith("workspace:"))
    .map(([name, range]) => `${field}.${name} = "${range}"`)
);

const userAgent = process.env.npm_config_user_agent ?? "";
if (workspaceRanges.length > 0 && !userAgent.startsWith("bun/")) {
  console.error(
    [
      `Refusing to publish ${manifest.name}@${manifest.version}: its manifest uses workspace ranges`,
      ...workspaceRanges.map((entry) => `  ${entry}`),
      `and it is being published by "${userAgent || "an unknown client"}", which would ship them unchanged`,
      "and leave the package uninstallable. Publish it with `bun publish` from",
      `${process.cwd()}, which replaces them with the sibling's real version.`,
    ].join("\n")
  );
  process.exit(1);
}
