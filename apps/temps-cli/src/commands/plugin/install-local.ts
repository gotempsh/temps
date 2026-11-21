// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from "commander";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { stat } from "node:fs/promises";
import { requireAuth, getApiUrl } from "../../config/store.js";
import { getErrorMessage, setupClient } from "../../lib/api-client.js";
import { installLocalFile } from "../../api/sdk.gen.js";
import { promptConfirm } from "../../ui/prompts.js";

export function registerLocalInstall(plugin: Command) {
  plugin
    .command("install-local <file>")
    .description(
      "Upload and activate a trusted native plugin on the configured Temps host",
    )
    .requiredOption("--name <name>", "Expected plugin manifest name")
    .requiredOption("--plugin-version <version>", "Expected plugin manifest version")
    .option("-y, --yes", "Approve execution of this file without prompting")
    .action(
      async (
        path: string,
        options: { name: string; pluginVersion: string; yes?: boolean },
      ) => {
        if (!options.yes && !process.stdin.isTTY)
          throw new Error(
            "Pass --yes to approve native plugin execution non-interactively.",
          );
        await requireAuth();
        await setupClient();
        const info = await stat(path);
        if (!info.isFile() || info.size === 0 || info.size > 256 * 1024 * 1024)
          throw new Error(
            "Choose a non-empty regular file of at most 256 MiB.",
          );
        const hash = createHash("sha256");
        for await (const chunk of createReadStream(path)) hash.update(chunk);
        const digest = hash.digest("hex");
        console.log(
          `Install ${options.name}@${options.pluginVersion} on ${getApiUrl()}\nSHA-256 ${digest}`,
        );
        if (
          !options.yes &&
          !(await promptConfirm({
            message:
              "Trust this executable to run native code on the Temps host?",
            default: false,
          }))
        )
          return;
        try {
          const { data } = await installLocalFile({
            query: { name: options.name, version: options.pluginVersion },
            headers: { "X-Temps-Trust-Sha256": digest },
            body: { file: new File([Bun.file(path)], "plugin") },
            throwOnError: true,
          });
          if (
            data.name !== options.name ||
            data.version !== options.pluginVersion ||
            data.sha256 !== digest
          )
            throw new Error(
              "Unexpected installation response. Check plugin status before retrying.",
            );
          console.log(data.message);
        } catch (error) {
          throw new Error(getErrorMessage(error));
        }
      },
    );
}
