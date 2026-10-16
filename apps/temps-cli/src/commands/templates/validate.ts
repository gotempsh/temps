// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { lstat, readdir, readFile } from "node:fs/promises";
import { basename, extname, join, relative, sep } from "node:path";

export interface NativeTemplateValidationResult {
  valid: boolean;
  errors: string[];
  templateCount: number;
}

const SUPPORTED_KINDS = new Set(["starter", "app", "service"]);
const SUPPORTED_CATALOG_VERSIONS = new Set(["2", "3"]);
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
// The slugs project creation accepts (temps_presets::all_presets() on the
// server, mirrored by temps_core::templates::VALID_PRESETS).
const SUPPORTED_PRESETS = new Set([
  "nextjs",
  "vite",
  "react-app",
  "rsbuild",
  "docusaurus",
  "rust",
  "go",
  "python",
  "java",
  "docker-compose",
  "dockerfile",
  "custom",
  "autopack",
  "nixpacks",
  "nixpacks-node",
  "nixpacks-python",
  "nixpacks-rust",
  "nixpacks-go",
  "nixpacks-java",
  "nixpacks-php",
  "nixpacks-ruby",
  "nixpacks-deno",
  "nixpacks-elixir",
  "nixpacks-csharp",
  "nixpacks-dart",
  "nixpacks-static",
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

const SERVICE_FIELDS = [
  "preset",
  "preset_config",
  "image",
  "command",
  "resources",
  "exposed_port",
  "health_check_path",
  "managed_service_bindings",
  "env_vars",
] as const;

const SERVICE_NAME = /^[a-z][a-z0-9-]{0,62}$/;

function hasControlCharacter(value: string): boolean {
  return Array.from(value).some((character) => {
    const code = character.codePointAt(0);
    return code !== undefined && (code <= 31 || code === 127);
  });
}

function isGitUrl(url: string): boolean {
  return (
    url.startsWith("http://") ||
    url.startsWith("https://") ||
    url.startsWith("git@")
  );
}

/**
 * The ref a source-built service builds, as the server resolves it: the
 * service's own `git` replaces the template's, and a `git` without `ref`
 * means `main`. Only an explicitly empty ref is unpinned.
 */
function pinnedSourceRef(serviceGit: unknown, templateGit: unknown): boolean {
  const git = isRecord(serviceGit) ? serviceGit : templateGit;
  if (!isRecord(git) || git.ref === undefined) return true;
  return typeof git.ref === "string" && git.ref.trim() !== "";
}

function isAppKind(kind: unknown): boolean {
  return kind === "app" || kind === "service";
}

/** Where a service's fields live in the document, for error messages. */
interface ServiceView {
  path: string;
  fields: Record<string, unknown>;
  bindingsKey: "managed_service_bindings" | "database_bindings";
  databasesKey: "services" | "databases";
  name: string;
}

/**
 * Variables: unique names, and no literal defaults on secrets.
 */
function validateVariables(
  variables: unknown,
  path: string,
  errors: string[],
  names: Set<string> = new Set(),
): Set<string> {
  if (variables === undefined) return names;
  if (!Array.isArray(variables)) {
    errors.push(`${path} must be an array`);
    return names;
  }
  variables.forEach((variable, variableIndex) => {
    if (!isRecord(variable)) {
      errors.push(`${path}[${variableIndex}] must be an object`);
      return;
    }
    const name = typeof variable.name === "string" ? variable.name.trim() : "";
    if (name === "") {
      errors.push(`${path}[${variableIndex}].name cannot be empty`);
    } else if (names.has(name)) {
      errors.push(`${path} name "${name}" is declared more than once`);
    } else {
      names.add(name);
    }
    if (
      isSecretEnvironmentVariable(variable) &&
      Object.prototype.hasOwnProperty.call(variable, "default")
    ) {
      errors.push(
        `${path}[${variableIndex}] is secret and cannot declare a literal default; use a secure generator or require user input`,
      );
    }
  });
  return names;
}

function validateService(
  service: ServiceView,
  template: Record<string, unknown>,
  databases: string[],
  multiService: boolean,
  errors: string[],
): void {
  const { path, fields } = service;
  const app = isAppKind(template.kind);
  const image = fields.image;
  const isImage = typeof image === "string" && image.trim() !== "";

  if (typeof fields.preset !== "string" || fields.preset.trim() === "") {
    errors.push(`${path}.preset is required`);
  } else if (!SUPPORTED_PRESETS.has(fields.preset)) {
    errors.push(`${path}.preset "${fields.preset}" is not supported`);
  }

  if (app) {
    if (isImage || !multiService) {
      if (fields.preset !== "dockerfile") {
        errors.push(`${path}.preset must be dockerfile for service templates`);
      }
      if (
        typeof image !== "string" ||
        utf8ByteLength(image) > 512 ||
        !pinnedImageReference(image)
      ) {
        errors.push(
          `${path}.image must be at most 512 bytes and use an immutable sha256 digest`,
        );
      }
      if (
        !Number.isInteger(fields.exposed_port) ||
        Number(fields.exposed_port) <= 0 ||
        Number(fields.exposed_port) > 65_535
      ) {
        errors.push(`${path}.exposed_port must be between 1 and 65535`);
      }
    } else if (!pinnedSourceRef(fields.git, template.git)) {
      // Source-built service of a multi-service app: an upgrade must be a
      // reviewable change to the reference, not whatever a branch moved to.
      errors.push(
        `${path} is built from source and must pin the git ref it builds (git.ref on the service or the template)`,
      );
    }
    if (fields.resources !== undefined) {
      if (!isRecord(fields.resources)) {
        errors.push(`${path}.resources must be an object`);
      } else {
        for (const field of [
          "cpu_request",
          "cpu_limit",
          "memory_request",
          "memory_limit",
        ]) {
          const resource = fields.resources[field];
          if (
            resource !== undefined &&
            (!Number.isInteger(resource) || Number(resource) <= 0)
          ) {
            errors.push(`${path}.resources.${field} must be a positive integer`);
          }
        }
        const request = fields.resources.memory_request;
        const limit = fields.resources.memory_limit;
        if (
          Number.isInteger(request) &&
          Number.isInteger(limit) &&
          Number(request) > Number(limit)
        ) {
          errors.push(
            `${path}.resources.memory_request must not exceed memory_limit`,
          );
        }
        const cpuRequest = fields.resources.cpu_request;
        const cpuLimit = fields.resources.cpu_limit;
        if (
          Number.isInteger(cpuRequest) &&
          Number.isInteger(cpuLimit) &&
          Number(cpuRequest) > Number(cpuLimit)
        ) {
          errors.push(`${path}.resources.cpu_request must not exceed cpu_limit`);
        }
      }
    }
  }

  if (fields.git !== undefined) {
    const url =
      isRecord(fields.git) && typeof fields.git.url === "string"
        ? fields.git.url.trim()
        : "";
    if (!isGitUrl(url)) {
      errors.push(`${path}.git.url must be an HTTP(S) or SSH Git URL`);
    }
  }
  if (fields.directory !== undefined) {
    const directory = fields.directory;
    if (
      typeof directory !== "string" ||
      directory.trim() === "" ||
      directory.startsWith("/") ||
      directory.split("/").includes("..") ||
      hasControlCharacter(directory)
    ) {
      errors.push(
        `${path}.directory must be a relative path inside the repository`,
      );
    }
  }

  if (fields.command !== undefined) {
    if (
      !Array.isArray(fields.command) ||
      fields.command.length === 0 ||
      fields.command.some(
        (part) => typeof part !== "string" || part.length === 0,
      )
    ) {
      errors.push(`${path}.command must contain non-empty arguments`);
    } else {
      const command = fields.command as string[];
      if (command.length > 64) {
        errors.push(`${path}.command cannot contain more than 64 arguments`);
      }
      if (
        command.some(
          (part) =>
            part.trim() === "" ||
            utf8ByteLength(part) > 1_024 ||
            hasControlCharacter(part),
        )
      ) {
        errors.push(
          `${path}.command arguments must be non-empty, at most 1024 bytes, and contain no control characters`,
        );
      }
    }
  }

  if (fields.health_check_path !== undefined) {
    const healthPath = fields.health_check_path;
    if (
      typeof healthPath !== "string" ||
      utf8ByteLength(healthPath) > 2_048 ||
      !healthPath.startsWith("/") ||
      healthPath.includes("@") ||
      healthPath.includes("://") ||
      hasControlCharacter(healthPath)
    ) {
      errors.push(
        `${path}.health_check_path must be a safe relative HTTP path starting with '/'`,
      );
    }
  }

  validateVariables(fields.env_vars, `${path}.env_vars`, errors);

  const bindingsKey = service.bindingsKey;
  const bindings = fields[bindingsKey];
  if (bindings !== undefined && !isRecord(bindings)) {
    errors.push(`${path}.${bindingsKey} must be an object`);
  } else if (isRecord(bindings)) {
    if (!app && Object.keys(bindings).length > 0) {
      errors.push(
        `${path}.${bindingsKey} are only applied for apps; a starter reads the database's variables directly`,
      );
    }
    for (const [database, aliases] of Object.entries(bindings)) {
      if (!databases.includes(database)) {
        errors.push(
          `${path}.${bindingsKey}.${database} must also be listed in ${service.databasesKey}`,
        );
      }
      if (!isRecord(aliases) || Object.keys(aliases).length === 0) {
        errors.push(
          `${path}.${bindingsKey}.${database} must contain environment aliases`,
        );
      } else if (
        Object.entries(aliases).some(
          ([target, source]) =>
            target.trim() === "" ||
            typeof source !== "string" ||
            source.trim() === "",
        )
      ) {
        errors.push(
          `${path}.${bindingsKey}.${database} aliases cannot be empty`,
        );
      }
    }
  }
  if (fields.databases !== undefined) {
    if (!Array.isArray(fields.databases)) {
      errors.push(`${path}.databases must be an array`);
    } else {
      for (const database of fields.databases) {
        if (typeof database !== "string" || !databases.includes(database)) {
          errors.push(
            `${path}.databases lists "${String(database)}", which the template's databases do not include`,
          );
        }
      }
    }
  }
}

/**
 * Fast, offline validation for contributors authoring Temps-native templates.
 * The server remains authoritative; this mirrors its source, preset,
 * managed-service, command, resource, service and route checks and adds a few
 * contributor-facing constraints such as pinned service images.
 *
 * Both template shapes are accepted: a list of service definitions under
 * `services` (ADR-051), or the original one-service shape whose fields sit at
 * the top level and whose `services` lists managed databases.
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
    : { version: "3", templates: [document] };

  if (!SUPPORTED_CATALOG_VERSIONS.has(String(catalog.version))) {
    errors.push('version must be "2" or "3"');
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
      errors.push(`${prefix}.kind must be starter or app`);
    }
    if (isAppKind(value.kind)) {
      if (typeof value.version !== "string" || value.version.trim() === "") {
        errors.push(`${prefix}.version is required for service templates`);
      } else if (!isSemanticVersion(value.version)) {
        errors.push(`${prefix}.version must use Semantic Versioning`);
      }
    }

    if (!isRecord(value.git)) {
      errors.push(`${prefix}.git is required`);
    } else {
      const gitUrl =
        typeof value.git.url === "string" ? value.git.url.trim() : "";
      if (gitUrl === "") {
        errors.push(`${prefix}.git.url is required`);
      } else if (!isGitUrl(gitUrl)) {
        errors.push(`${prefix}.git.url must be an HTTP(S) or SSH Git URL`);
      }
    }

    if (value.services !== undefined && !Array.isArray(value.services)) {
      errors.push(`${prefix}.services must be an array`);
    }
    const rawServices = Array.isArray(value.services) ? value.services : [];
    const listsDefinitions =
      rawServices.length > 0 && rawServices.every((entry) => isRecord(entry));

    let databases: string[];
    let databasesPath: string;
    let services: ServiceView[];
    if (listsDefinitions) {
      for (const field of SERVICE_FIELDS) {
        if (value[field] !== undefined) {
          errors.push(
            `${prefix}.${field} belongs to one of the services, not to the template`,
          );
        }
      }
      databases = Array.isArray(value.databases)
        ? value.databases.map(String)
        : [];
      databasesPath = `${prefix}.databases`;
      if (value.databases !== undefined && !Array.isArray(value.databases)) {
        errors.push(`${prefix}.databases must be an array`);
      }
      const names = new Set<string>();
      services = rawServices.map((entry, serviceIndex) => {
        const fields = entry as Record<string, unknown>;
        const path = `${prefix}.services[${serviceIndex}]`;
        const name = typeof fields.name === "string" ? fields.name : "";
        if (!SERVICE_NAME.test(name) || name.endsWith("-")) {
          errors.push(
            `${path}.name must be 1-63 lowercase letters, digits or '-', starting with a letter`,
          );
        } else if (names.has(name)) {
          errors.push(`${path}.name duplicates "${name}"`);
        } else {
          names.add(name);
        }
        return {
          path,
          fields,
          bindingsKey: "database_bindings",
          databasesKey: "databases",
          name,
        };
      });
    } else {
      rawServices.forEach((service) => {
        if (
          typeof service !== "string" ||
          !SUPPORTED_MANAGED_SERVICES.has(service)
        ) {
          errors.push(
            `${prefix}.services contains unsupported service "${String(service)}"`,
          );
        }
      });
      databases = rawServices.filter(
        (entry): entry is string => typeof entry === "string",
      );
      databasesPath = `${prefix}.services`;
      services = [
        {
          path: prefix,
          fields: value,
          bindingsKey: "managed_service_bindings",
          databasesKey: "services",
          name: slug,
        },
      ];
    }
    if (listsDefinitions) {
      databases.forEach((database) => {
        if (!SUPPORTED_MANAGED_SERVICES.has(database)) {
          errors.push(
            `${databasesPath} contains unsupported service "${database}"`,
          );
        }
      });
    }

    const multiService = services.length > 1;
    services.forEach((service) =>
      validateService(service, value, databases, multiService, errors),
    );

    // Every database must be linked to at least one service.
    if (listsDefinitions && multiService) {
      for (const database of databases) {
        const linked = services.some((service) => {
          const own = Array.isArray(service.fields.databases)
            ? service.fields.databases
            : [];
          const bindings = isRecord(service.fields.database_bindings)
            ? Object.keys(service.fields.database_bindings)
            : [];
          return own.includes(database) || bindings.includes(database);
        });
        if (!linked) {
          errors.push(
            `${databasesPath} lists "${database}", but no service uses it; list it in a service's databases or database_bindings`,
          );
        }
      }
    }

    const shared = validateVariables(
      value.shared_env_vars,
      `${prefix}.shared_env_vars`,
      errors,
    );
    for (const service of services) {
      if (service.path === prefix) continue;
      const own = Array.isArray(service.fields.env_vars)
        ? service.fields.env_vars
        : [];
      for (const variable of own) {
        if (isRecord(variable) && shared.has(String(variable.name))) {
          errors.push(
            `${service.path}.env_vars "${String(variable.name)}" is also a shared variable`,
          );
        }
      }
    }

    if (value.routes !== undefined) {
      if (!Array.isArray(value.routes)) {
        errors.push(`${prefix}.routes must be an array`);
      } else {
        const names = new Set(services.map((service) => service.name));
        const seen = new Set<string>();
        value.routes.forEach((route, routeIndex) => {
          const path = `${prefix}.routes[${routeIndex}]`;
          if (!isRecord(route)) {
            errors.push(`${path} must be an object`);
            return;
          }
          for (const role of ["source", "target"] as const) {
            if (typeof route[role] !== "string" || !names.has(route[role])) {
              errors.push(`${path}.${role} must name a service of the template`);
            }
          }
          if (route.source === route.target) {
            errors.push(`${path} points at the service it is on`);
          }
          const prefixValue = route.prefix;
          if (
            typeof prefixValue !== "string" ||
            prefixValue.length < 2 ||
            !prefixValue.startsWith("/") ||
            prefixValue.endsWith("/") ||
            prefixValue.includes("//") ||
            /\s/.test(prefixValue) ||
            hasControlCharacter(prefixValue)
          ) {
            errors.push(`${path}.prefix must be a path such as /api`);
          }
          const key = `${String(route.source)} ${String(prefixValue)}`;
          if (seen.has(key)) {
            errors.push(`${path} repeats ${String(prefixValue)} on ${String(route.source)}`);
          }
          seen.add(key);
        });
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
          ? "app"
          : directory === "starters"
            ? "starter"
            : undefined;
      const declaredKind = document.kind === "service" ? "app" : document.kind;
      if (!expectedKind) {
        errors.push(`${filePath} must be under services/ or starters/`);
      } else if (declaredKind !== expectedKind) {
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

  const result = validateNativeTemplateConfig({ version: "3", templates });
  return {
    valid: errors.length === 0 && result.valid,
    errors: [...errors, ...result.errors],
    templateCount: templates.length,
  };
}
