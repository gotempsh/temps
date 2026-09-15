// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Command } from "commander";
import { requireAuth } from "../../config/store.js";
import { client, setupClient, getErrorMessage } from "../../lib/api-client.js";
import { promptConfirm } from "../../ui/prompts.js";

type Options = { name?: string; ref?: string; yes?: boolean };
type InstallBody = { repository_url: string; name?: string; ref_name?: string };
type Result = {
  name: string;
  version: string;
  source_commit: string;
  message: string;
};

export class PluginInstallError extends Error {
  override name = "PluginInstallError";
}

export function installBody(repository: string, options: Options): InstallBody {
  if (
    !/^https:\/\/github\.com\/[A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(
      repository,
    )
  )
    throw new PluginInstallError(
      "Use https://github.com/owner/repository without embedded credentials.",
    );
  if (
    options.name !== undefined &&
    !/^[a-z0-9][a-z0-9-]{0,63}$/.test(options.name)
  )
    throw new PluginInstallError(
      "The optional plugin name must match the name declared by its package.",
    );
  validateRef(options.ref);
  return {
    repository_url: repository,
    ...(options.name === undefined ? {} : { name: options.name }),
    ...(options.ref === undefined ? {} : { ref_name: options.ref }),
  };
}

function validateRef(ref: string | undefined) {
  if (
    ref !== undefined &&
    (ref.length > 128 ||
      !/^[A-Za-z0-9][A-Za-z0-9_./-]*$/.test(ref) ||
      ref.includes(".."))
  )
    throw new PluginInstallError(
      "Enter a valid branch, tag, or commit for --ref.",
    );
}

export function validateInstallResult(value: unknown): Result {
  if (
    typeof value !== "object" ||
    value === null ||
    !("name" in value) ||
    typeof value.name !== "string" ||
    !("version" in value) ||
    typeof value.version !== "string" ||
    !("source_commit" in value) ||
    typeof value.source_commit !== "string" ||
    !/^[a-f0-9]{40}$/.test(value.source_commit) ||
    !("message" in value) ||
    typeof value.message !== "string"
  )
    throw new PluginInstallError(
      "Temps returned an invalid plugin installation response. Check plugin status before retrying.",
    );
  return {
    name: value.name,
    version: value.version,
    source_commit: value.source_commit,
    message: value.message,
  };
}

async function send(url: string, body: InstallBody | { ref_name?: string }) {
  await requireAuth();
  await setupClient();
  try {
    const response = await client.post<{ 200: unknown }, unknown, true>({
      url,
      body,
      headers: { "Content-Type": "application/json" },
      throwOnError: true,
    });
    const result = validateInstallResult(response.data);
    console.log(
      `Installed ${result.name}@${result.version} (${result.source_commit.slice(0, 12)}). Plugins reloaded automatically.`,
    );
  } catch (error) {
    throw new PluginInstallError(getErrorMessage(error));
  }
}

async function confirmTrust(yes?: boolean) {
  if (yes) return true;
  if (!process.stdin.isTTY)
    throw new PluginInstallError(
      "Installation executes trusted repository code on the Temps host. Pass --yes to confirm non-interactively.",
    );
  return promptConfirm({
    message:
      "Trust this repository and its dependencies? Docker isolates the build, but installed plugins run with the Temps host’s permissions.",
    default: false,
  });
}

export function registerPluginInstallCommands(plugin: Command) {
  plugin
    .command("install <repository>")
    .description(
      "Install a GitHub TypeScript plugin on the configured Temps server; the server uses its host Git credentials and Docker",
    )
    .option(
      "--name <name>",
      "Advanced: require this plugin name (otherwise auto-detected)",
    )
    .option(
      "--ref <ref>",
      "Advanced: branch, tag, or commit (otherwise repository default branch)",
    )
    .option(
      "-y, --yes",
      "Trust the repository and allow installation without prompting",
    )
    .action(async (repository: string, options: Options) => {
      const body = installBody(repository, options);
      if (await confirmTrust(options.yes))
        await send("/x/plugins/install/repository", body);
    });
  plugin
    .command("update <name>")
    .description(
      "Rebuild an installed GitHub plugin from its stored source; keep the current plugin if the update fails",
    )
    .option("--ref <ref>", "Use a different branch, tag, or commit")
    .option("-y, --yes", "Trust the update without prompting")
    .action(async (name: string, options: Options) => {
      if (!/^[a-z0-9][a-z0-9-]{0,63}$/.test(name))
        throw new PluginInstallError("Enter a valid installed plugin name.");
      validateRef(options.ref);
      if (await confirmTrust(options.yes))
        await send(
          `/x/plugins/${encodeURIComponent(name)}/update`,
          options.ref === undefined ? {} : { ref_name: options.ref },
        );
    });
}
