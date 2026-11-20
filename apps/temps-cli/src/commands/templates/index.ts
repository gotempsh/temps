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
import { createTemplateAction } from "./create.js";

export function registerTemplatesCommands(program: Command): void {
  const cmd = program
    .command("templates")
    .alias("tpl")
    .description("Browse templates and create them");

  cmd
    .command("list")
    .alias("ls")
    .description("List available templates")
    .option("--json", "Output in JSON format")
    .option("--kind <kind>", "Filter by kind: starter or app")
    .action(listTemplatesAction);

  cmd
    .command("create <slug>")
    .description(
      "Create a template: every service (an app, what `projects` manages), the databases it needs, its shared variables and path routes",
    )
    .option(
      "--project <name>",
      "Put the services in a new project (group) with this name; the default for several services",
    )
    .option("--into <project>", "Add the services to an existing project (group), by slug or id (see: project-groups list)")
    .option("--standalone", "Create a one-service template without a project (group); the default for one service")
    .option("--name <service=name...>", "Name a created service; a one-service template also takes a bare name")
    .option("--env <[service:]KEY=VALUE...>", "Set a variable; prefix the service when the template cannot tell")
    .option("--secret <[service:]KEY=VALUE...>", "Set a secret variable (write-only once stored)")
    .option("--database <type=new|type=new:name|type=id...>", "Create or link each database the template needs (default: create)")
    .option("--fork-connection <id>", "Starters only: copy the code into a new repository on this git connection")
    .option("--repo-name <name>", "Name of the new repository (with --fork-connection)")
    .option("--repo-owner <owner>", "Account or organization for the new repository")
    .option("--public-repo", "Make the new repository public (private by default)")
    .option("--no-auto-deploy", "Do not deploy on push to the new repository")
    .option("--idempotency-key <key>", "Reuse a key to get the same result instead of creating twice")
    .option("--no-wait", "Return right away instead of waiting until it finishes")
    .option("--timeout <minutes>", "Stop waiting after this long (default 30); it keeps running on the server")
    .option("--json", "Output in JSON format")
    .action(createTemplateAction);

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
 * --kind is compared case-insensitively so `--kind App` matches templates
 * whose kind is stored as "app" — otherwise a script would get a silent empty
 * result instead of the templates it expected. "service" is the former name of
 * "app" and still selects apps, whichever of the two the server reports.
 */
export function filterTemplatesByKind(
  templates: TemplateResponse[],
  kind: string | undefined,
): TemplateResponse[] {
  if (!kind) return templates;
  const normalize = (value: string): string => {
    const lower = value.toLowerCase();
    return lower === "service" ? "app" : lower;
  };
  const normalizedKind = normalize(kind);
  if (normalizedKind !== "starter" && normalizedKind !== "app") {
    throw new Error(
      `Invalid template kind "${kind}". Expected "starter" or "app".`,
    );
  }
  return templates.filter(
    (template) => normalize(String(template.kind)) === normalizedKind,
  );
}

export function formatTemplatePort(port: number | null | undefined): string {
  return port != null ? String(port) : "-";
}
