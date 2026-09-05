// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { lstat, readdir, readFile } from "node:fs/promises";
import { basename, extname, join, relative, sep } from "node:path";

export interface NativeTemplateValidationResult {
  valid: boolean;
  errors: string[];
  templateCount: number;
}

const SUPPORTED_KINDS = new Set(["starter", "service"]);
const MAX_TEMPLATE_DIRECTORY_DEPTH = 16;
const MAX_TEMPLATE_FILES = 1_000;
const MAX_TEMPLATE_YAML_BYTES = 1024 * 1024;
const utf8ByteLength = (value: string): number =>
  new TextEncoder().encode(value).length;
const SUPPORTED_MANAGED_SERVICES = new Set([
  "postgres",
  "mariadb",
  "redis",
  "mongodb",
  "s3",
  "kv",
  "blob",
  "rustfs",
]);
const SUPPORTED_PRESETS = new Set([
  "nextjs",
  "vite",
  "astro",
  "nuxt",
  "remix",
  "sveltekit",
  "solidstart",
  "angular",
  "vue",
  "react",
  "docusaurus",
  "rsbuild",
  "python",
  "fastapi",
  "flask",
  "django",
  "rails",
  "go",
  "rust",
  "java",
  "laravel",
  "dockerfile",
  "nixpacks",
  "autopack",
  "static",
  "docker-compose",
  "nodejs",
]);
const SEMANTIC_VERSION =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

function isSemanticVersion(version: string): boolean {
  if (!SEMANTIC_VERSION.test(version)) return false;
  const [withoutBuild = version] = version.split("+", 1);
  const prereleaseStart = withoutBuild.indexOf("-");
  if (prereleaseStart < 0) return true;
  return withoutBuild
    .slice(prereleaseStart + 1)
    .split(".")
    .every(
      (identifier) =>
        !/^\d+$/.test(identifier) ||
        identifier === "0" ||
        !identifier.startsWith("0"),
    );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function pinnedImageReference(image: string): boolean {
  const digestSeparator = image.lastIndexOf("@");
  return (
    digestSeparator > 0 &&
    /^sha256:[0-9a-f]{64}$/i.test(image.slice(digestSeparator + 1))
  );
}

function isSecretEnvironmentVariable(
  variable: Record<string, unknown>,
): boolean {
  if (variable.secret === true) return true;
  const generator = variable.default_generator;
  if (typeof generator === "string" && generator.includes("secret")) {
    return true;
  }

  const name =
    typeof variable.name === "string" ? variable.name.toUpperCase() : "";
  const secretSegment = name
    .split("_")
    .some((segment) =>
      ["SECRET", "PASSWORD", "PASSWD", "TOKEN", "PRIVATEKEY"].includes(segment),
    );
  const secretSuffixes = [
    "_API_KEY",
    "_PRIVATE_KEY",
    "_ACCESS_KEY",
    "_DATABASE_URL",
    "_POSTGRES_URL",
    "_MYSQL_URL",
    "_MONGODB_URL",
    "_MONGODB_URI",
    "_REDIS_URL",
    "_AMQP_URL",
    "_CONNECTION_STRING",
    "_DSN",
    "_WEBHOOK_URL",
  ];
  const exactSecretNames = new Set([
    "DATABASE_URL",
    "POSTGRES_URL",
    "MYSQL_URL",
    "MONGODB_URL",
    "MONGODB_URI",
    "REDIS_URL",
    "AMQP_URL",
    "CONNECTION_STRING",
    "DSN",
    "WEBHOOK_URL",
  ]);
  return (
    secretSegment ||
    secretSuffixes.some((suffix) => name.endsWith(suffix)) ||
    exactSecretNames.has(name)
  );
}

/**
 * Fast, offline validation for contributors authoring Temps-native service
 * templates. The server remains authoritative; this mirrors its required
 * source, preset, managed-service, command, and resource checks and adds a few
 * contributor-facing constraints such as pinned service images.
 */
export function validateNativeTemplateConfig(
  document: unknown,
): NativeTemplateValidationResult {
  const errors: string[] = [];
  if (!isRecord(document)) {
    return {
      valid: false,
      errors: ["Document must be a YAML object"],
      templateCount: 0,
    };
  }

  const catalog = Array.isArray(document.templates)
    ? document
    : { version: "2", templates: [document] };

  if (catalog.version !== "2") {
    errors.push('version must be "2"');
  }
  if (!Array.isArray(catalog.templates)) {
    errors.push("templates must be an array");
    return { valid: false, errors, templateCount: 0 };
  }

  const seenSlugs = new Set<string>();
  catalog.templates.forEach((value, index) => {
    const prefix = `templates[${index}]`;
    if (!isRecord(value)) {
      errors.push(`${prefix} must be an object`);
      return;
    }

    const slug = typeof value.slug === "string" ? value.slug.trim() : "";
    if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(slug)) {
      errors.push(`${prefix}.slug must be lowercase kebab-case`);
    } else if (seenSlugs.has(slug)) {
      errors.push(`${prefix}.slug duplicates "${slug}"`);
    } else {
      seenSlugs.add(slug);
    }
    if (typeof value.name !== "string" || value.name.trim() === "") {
      errors.push(`${prefix}.name is required`);
    }
    if (typeof value.kind !== "string" || !SUPPORTED_KINDS.has(value.kind)) {
      errors.push(`${prefix}.kind must be starter or service`);
    }
    if (value.kind === "service") {
      if (typeof value.version !== "string" || value.version.trim() === "") {
        errors.push(`${prefix}.version is required for service templates`);
      } else if (!isSemanticVersion(value.version)) {
        errors.push(`${prefix}.version must use Semantic Versioning`);
      }
      if (value.preset !== "dockerfile") {
        errors.push(
          `${prefix}.preset must be dockerfile for service templates`,
        );
      }
    }

    if (!isRecord(value.git)) {
      errors.push(`${prefix}.git is required`);
    } else {
      const gitUrl =
        typeof value.git.url === "string" ? value.git.url.trim() : "";
      if (gitUrl === "") {
        errors.push(`${prefix}.git.url is required`);
      } else if (
        !gitUrl.startsWith("http://") &&
        !gitUrl.startsWith("https://") &&
        !gitUrl.startsWith("git@")
      ) {
        errors.push(`${prefix}.git.url must be an HTTP(S) or SSH Git URL`);
      }
    }

    if (typeof value.preset !== "string" || value.preset.trim() === "") {
      errors.push(`${prefix}.preset is required`);
    } else if (!SUPPORTED_PRESETS.has(value.preset)) {
      errors.push(`${prefix}.preset "${value.preset}" is not supported`);
    }

    if (value.command !== undefined) {
      if (
        !Array.isArray(value.command) ||
        value.command.length === 0 ||
        value.command.some(
          (part) => typeof part !== "string" || part.length === 0,
        )
      ) {
        errors.push(`${prefix}.command must contain non-empty arguments`);
      } else {
        const command = value.command as string[];
        if (command.length > 64) {
          errors.push(
            `${prefix}.command cannot contain more than 64 arguments`,
          );
        }
        if (
          command.some(
            (part) =>
              part.trim() === "" ||
              utf8ByteLength(part) > 1_024 ||
              Array.from(part).some((character) => {
                const code = character.codePointAt(0);
                return code !== undefined && (code <= 31 || code === 127);
              }),
          )
        ) {
          errors.push(
            `${prefix}.command arguments must be non-empty, at most 1024 bytes, and contain no control characters`,
          );
        }
      }
    }

    if (value.health_check_path !== undefined) {
      const path = value.health_check_path;
      if (
        typeof path !== "string" ||
        utf8ByteLength(path) > 2_048 ||
        !path.startsWith("/") ||
        path.includes("@") ||
        path.includes("://") ||
        Array.from(path).some((character) => {
          const code = character.codePointAt(0);
          return code !== undefined && (code <= 31 || code === 127);
        })
      ) {
        errors.push(
          `${prefix}.health_check_path must be a safe relative HTTP path starting with '/'`,
        );
      }
    }

    if (value.kind === "service") {
      if (
        typeof value.image !== "string" ||
        utf8ByteLength(value.image) > 512 ||
        !pinnedImageReference(value.image)
      ) {
        errors.push(
          `${prefix}.image must be at most 512 bytes and use an immutable sha256 digest`,
        );
      }
      if (value.resources !== undefined) {
        if (!isRecord(value.resources)) {
          errors.push(`${prefix}.resources must be an object`);
        } else {
          for (const field of [
            "cpu_request",
            "cpu_limit",
            "memory_request",
            "memory_limit",
          ]) {
            const resource = value.resources[field];
            if (
              resource !== undefined &&
              (!Number.isInteger(resource) || Number(resource) <= 0)
            ) {
              errors.push(
                `${prefix}.resources.${field} must be a positive integer`,
              );
            }
          }
          const request = value.resources.memory_request;
          const limit = value.resources.memory_limit;
          if (
            Number.isInteger(request) &&
            Number.isInteger(limit) &&
            Number(request) > Number(limit)
          ) {
            errors.push(
              `${prefix}.resources.memory_request must not exceed memory_limit`,
            );
          }
          const cpuRequest = value.resources.cpu_request;
          const cpuLimit = value.resources.cpu_limit;
          if (
            Number.isInteger(cpuRequest) &&
            Number.isInteger(cpuLimit) &&
            Number(cpuRequest) > Number(cpuLimit)
          ) {
            errors.push(
              `${prefix}.resources.cpu_request must not exceed cpu_limit`,
            );
          }
        }
      }
      if (
        !Number.isInteger(value.exposed_port) ||
        Number(value.exposed_port) <= 0 ||
        Number(value.exposed_port) > 65_535
      ) {
        errors.push(`${prefix}.exposed_port must be between 1 and 65535`);
      }
    }

    if (value.services !== undefined && !Array.isArray(value.services)) {
      errors.push(`${prefix}.services must be an array`);
    }
    const services = Array.isArray(value.services) ? value.services : [];
    services.forEach((service) => {
      if (
        typeof service !== "string" ||
        !SUPPORTED_MANAGED_SERVICES.has(service)
      ) {
        errors.push(
          `${prefix}.services contains unsupported service "${String(service)}"`,
        );
      }
    });

    if (value.env_vars !== undefined && !Array.isArray(value.env_vars)) {
      errors.push(`${prefix}.env_vars must be an array`);
    } else if (Array.isArray(value.env_vars)) {
      const environmentVariableNames = new Set<string>();
      value.env_vars.forEach((variable, variableIndex) => {
        if (!isRecord(variable)) {
          errors.push(`${prefix}.env_vars[${variableIndex}] must be an object`);
          return;
        }
        const name =
          typeof variable.name === "string" ? variable.name.trim() : "";
        if (name === "") {
          errors.push(
            `${prefix}.env_vars[${variableIndex}].name cannot be empty`,
          );
        } else if (environmentVariableNames.has(name)) {
          errors.push(
            `${prefix}.env_vars name "${name}" is declared more than once`,
          );
        } else {
          environmentVariableNames.add(name);
        }
        if (
          isSecretEnvironmentVariable(variable) &&
          Object.prototype.hasOwnProperty.call(variable, "default")
        ) {
          errors.push(
            `${prefix}.env_vars[${variableIndex}] is secret and cannot declare a literal default; use a secure generator or require user input`,
          );
        }
      });
    }

    if (
      value.managed_service_bindings !== undefined &&
      !isRecord(value.managed_service_bindings)
    ) {
      errors.push(`${prefix}.managed_service_bindings must be an object`);
    } else if (isRecord(value.managed_service_bindings)) {
      for (const [service, bindings] of Object.entries(
        value.managed_service_bindings,
      )) {
        if (!services.includes(service)) {
          errors.push(
            `${prefix}.managed_service_bindings.${service} must also be listed in services`,
          );
        }
        if (!isRecord(bindings) || Object.keys(bindings).length === 0) {
          errors.push(
            `${prefix}.managed_service_bindings.${service} must contain environment aliases`,
          );
        } else if (
          Object.entries(bindings).some(
            ([target, source]) =>
              target.trim() === "" ||
              typeof source !== "string" ||
              source.trim() === "",
          )
        ) {
          errors.push(
            `${prefix}.managed_service_bindings.${service} aliases cannot be empty`,
          );
        }
      }
    }
  });

  return {
    valid: errors.length === 0,
    errors,
    templateCount: catalog.templates.length,
  };
}

async function yamlFilesIn(
  path: string,
  depth = 0,
  files: string[] = [],
): Promise<string[]> {
  if (depth > MAX_TEMPLATE_DIRECTORY_DEPTH) {
    throw new Error(
      `Template directory exceeds the maximum depth of ${MAX_TEMPLATE_DIRECTORY_DEPTH}`,
    );
  }

  const metadata = await lstat(path);
  if (metadata.isSymbolicLink()) {
    throw new Error(
      `Symbolic links are not allowed in template paths: ${path}`,
    );
  }
  if (metadata.isFile()) {
    if (extname(path) !== ".yaml") return files;
    if (metadata.size > MAX_TEMPLATE_YAML_BYTES) {
      throw new Error(
        `Template YAML exceeds the ${MAX_TEMPLATE_YAML_BYTES}-byte limit: ${path}`,
      );
    }
    if (files.length >= MAX_TEMPLATE_FILES) {
      throw new Error(
        `Template directory exceeds the ${MAX_TEMPLATE_FILES}-file limit`,
      );
    }
    files.push(path);
    return files;
  }
  if (!metadata.isDirectory()) return files;

  const entries = await readdir(path, { withFileTypes: true });
  entries.sort((left, right) => left.name.localeCompare(right.name));
  for (const entry of entries) {
    await yamlFilesIn(join(path, entry.name), depth + 1, files);
  }
  return files;
}

export async function readAndValidateTemplatePath(
  path: string,
): Promise<NativeTemplateValidationResult> {
  let files: string[];
  try {
    files = await yamlFilesIn(path);
  } catch (error) {
    return {
      valid: false,
      errors: [
        `Cannot read template path ${path}: ${error instanceof Error ? error.message : String(error)}`,
      ],
      templateCount: 0,
    };
  }

  if (files.length === 0) {
    return {
      valid: false,
      errors: [`No .yaml template files found at: ${path}`],
      templateCount: 0,
    };
  }

  if (files.length === 1 && files[0] === path) {
    try {
      return validateNativeTemplateConfig(
        Bun.YAML.parse(await readFile(path, "utf8")),
      );
    } catch (error) {
      return {
        valid: false,
        errors: [
          `Invalid YAML: ${error instanceof Error ? error.message : String(error)}`,
        ],
        templateCount: 0,
      };
    }
  }

  const templates: unknown[] = [];
  const errors: string[] = [];
  for (const file of files) {
    const filePath = relative(path, file).split(sep).join("/");
    try {
      const document = Bun.YAML.parse(await readFile(file, "utf8"));
      if (!isRecord(document) || Array.isArray(document.templates)) {
        errors.push(`${filePath} must contain one template object directly`);
        continue;
      }
      const directory = filePath.split("/", 1)[0];
      const expectedKind =
        directory === "services"
          ? "service"
          : directory === "starters"
            ? "starter"
            : undefined;
      if (!expectedKind) {
        errors.push(`${filePath} must be under services/ or starters/`);
      } else if (document.kind !== expectedKind) {
        errors.push(
          `${filePath} declares kind "${String(document.kind)}" but its directory requires "${expectedKind}"`,
        );
      }
      const expectedFilename =
        typeof document.slug === "string" ? `${document.slug}.yaml` : undefined;
      if (expectedFilename && basename(file) !== expectedFilename) {
        errors.push(
          `${filePath} must use its slug as the filename (${expectedFilename})`,
        );
      }
      templates.push(document);
    } catch (error) {
      errors.push(
        `${filePath}: invalid YAML: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }

  const result = validateNativeTemplateConfig({ version: "2", templates });
  return {
    valid: errors.length === 0 && result.valid,
    errors: [...errors, ...result.errors],
    templateCount: templates.length,
  };
}
