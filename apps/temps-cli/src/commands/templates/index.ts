// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from "commander";
import { requireAuth } from "../../config/store.js";
import { setupClient, client, getErrorMessage } from "../../lib/api-client.js";
import { listProjectTemplates } from "../../api/sdk.gen.js";
import type { TemplateResponse } from "../../api/types.gen.js";
import { withSpinner } from "../../ui/spinner.js";
import { printTable, type TableColumn } from "../../ui/table.js";
import { newline, header, icons, json, colors, info } from "../../ui/output.js";
import { readAndValidateTemplatePath } from "./validate.js";

export function registerTemplatesCommands(program: Command): void {
  const cmd = program
    .command("templates")
    .alias("tpl")
    .description("Browse deployment templates");

  cmd
    .command("list")
    .alias("ls")
    .description("List available templates")
    .option("--json", "Output in JSON format")
    .option("--kind <kind>", "Filter by template gallery (starter, service)")
    .action(listTemplatesAction);

  cmd
    .command("validate <path>")
    .description(
      "Validate a Temps-native template YAML file or directory offline",
    )
    .option("--json", "Output in JSON format")
    .action(async (path: string, options: { json?: boolean }) => {
      const result = await readAndValidateTemplatePath(path);
      if (options.json) {
        json(result);
      } else if (result.valid) {
        info(
          `Valid native template catalog (${result.templateCount} template(s))`,
        );
      } else {
        result.errors.forEach((error) => info(`- ${error}`));
      }
      if (!result.valid) process.exitCode = 1;
    });
}

async function listTemplatesAction(options: {
  json?: boolean;
  kind?: string;
}): Promise<void> {
  await requireAuth();
  await setupClient();

  const templatesData = await withSpinner("Fetching templates...", async () => {
    const { data, error } = await listProjectTemplates({ client });
    if (error) {
      throw new Error(getErrorMessage(error));
    }
    return data;
  });

  const templates = filterTemplatesByKind(
    templatesData?.templates ?? [],
    options.kind,
  );

  if (options.json) {
    json(templates);
    return;
  }

  newline();
  header(`${icons.package} Available Templates (${templates.length})`);

  if (templates.length === 0) {
    info("No templates found");
    newline();
    return;
  }

  const columns: TableColumn<TemplateResponse>[] = [
    { header: "Slug", key: "slug", color: (v) => colors.bold(v) },
    { header: "Name", key: "name" },
    { header: "Kind", key: "kind", color: (v) => colors.primary(v) },
    {
      header: "Version",
      accessor: (template) => template.version || "-",
      color: (v) => (v === "-" ? colors.muted(v) : v),
    },
    {
      header: "Port",
      accessor: (template) => formatTemplatePort(template.exposed_port),
      color: (v) => (v === "-" ? colors.muted(v) : v),
    },
    {
      header: "Description",
      key: "description",
      color: (v) => colors.muted(v),
    },
  ];

  printTable(templates, columns, { style: "minimal" });
  newline();
}

/**
 * --kind is compared case-insensitively so `--kind Service` matches templates
 * whose kind is stored as "service" — otherwise a script would get a
 * silent empty result instead of the templates it expected.
 */
export function filterTemplatesByKind(
  templates: TemplateResponse[],
  kind: string | undefined,
): TemplateResponse[] {
  if (!kind) return templates;
  const normalizedKind = kind.toLowerCase();
  if (normalizedKind !== "starter" && normalizedKind !== "service") {
    throw new Error(
      `Invalid template kind "${kind}". Expected "starter" or "service".`,
    );
  }
  return templates.filter(
    (template) => template.kind.toLowerCase() === normalizedKind,
  );
}

export function formatTemplatePort(port: number | null | undefined): string {
  return port != null ? String(port) : "-";
}
