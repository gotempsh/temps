// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from "bun:test";
import { validateNativeTemplateConfig } from "./validate.js";

const PINNED_IMAGE = `example.test/app@sha256:${"a".repeat(64)}`;

describe("validateNativeTemplateConfig", () => {
  test("accepts a pinned PostgreSQL-backed service", () => {
    const result = validateNativeTemplateConfig({
      version: "2",
      templates: [
        {
          slug: "keycloak",
          name: "Keycloak",
          kind: "service",
          version: "1.0.0",
          git: {
            url: "https://github.com/keycloak/keycloak.git",
            ref: "26.7.2",
          },
          preset: "dockerfile",
          image: PINNED_IMAGE,
          exposed_port: 8080,
          resources: {
            cpu_request: 500000,
            memory_request: 512,
            memory_limit: 1536,
          },
          services: ["postgres"],
          managed_service_bindings: {
            postgres: { KC_DB_USERNAME: "POSTGRES_USER" },
          },
        },
      ],
    });

    expect(result).toEqual({ valid: true, errors: [], templateCount: 1 });
  });

  test("rejects floating images and undeclared bindings", () => {
    const result = validateNativeTemplateConfig({
      version: "2",
      templates: [
        {
          slug: "bad-service",
          name: "Bad service",
          kind: "service",
          version: "1.0.0",
          git: { url: "https://example.test/bad-service.git" },
          preset: "dockerfile",
          image: "example/app:latest",
          exposed_port: 3000,
          services: [],
          managed_service_bindings: {
            postgres: { DATABASE_URL: "POSTGRES_URL" },
          },
        },
      ],
    });

    expect(result.valid).toBeFalse();
    expect(result.errors).toContain(
      "templates[0].image must use an immutable sha256 digest",
    );
    expect(result.errors).toContain(
      "templates[0].managed_service_bindings.postgres must also be listed in services",
    );
  });

  test("does not mistake a registry port for an image version", () => {
    const result = validateNativeTemplateConfig({
      version: "2",
      templates: [
        {
          slug: "registry-port",
          name: "Registry port",
          kind: "service",
          version: "1.0.0",
          git: { url: "https://example.test/registry-port.git" },
          preset: "dockerfile",
          image: "registry.example:5000/app",
          exposed_port: 3000,
        },
      ],
    });

    expect(result.valid).toBeFalse();
    expect(result.errors).toContain(
      "templates[0].image must use an immutable sha256 digest",
    );
  });

  test("rejects mutable version tags for curated services", () => {
    const result = validateNativeTemplateConfig({
      version: "2",
      templates: [
        {
          slug: "mutable-version",
          name: "Mutable version",
          kind: "service",
          version: "1.0.0",
          git: { url: "https://example.test/mutable-version.git" },
          preset: "dockerfile",
          image: "example.test/app:1.0.0",
          exposed_port: 3000,
        },
      ],
    });

    expect(result.valid).toBeFalse();
    expect(result.errors).toContain(
      "templates[0].image must use an immutable sha256 digest",
    );
  });

  test("accepts a pinned sha256 image digest", () => {
    const result = validateNativeTemplateConfig({
      version: "2",
      templates: [
        {
          slug: "digest-image",
          name: "Digest image",
          kind: "service",
          version: "1.0.0",
          git: { url: "https://example.test/digest-image.git" },
          preset: "dockerfile",
          image: `registry.example:5000/app@sha256:${"a".repeat(64)}`,
          exposed_port: 3000,
        },
      ],
    });

    expect(result).toEqual({ valid: true, errors: [], templateCount: 1 });
  });

  test("rejects scalar managed service collections", () => {
    const result = validateNativeTemplateConfig({
      version: "2",
      templates: [
        {
          slug: "scalar-services",
          name: "Scalar services",
          kind: "service",
          version: "1.0.0",
          git: { url: "https://example.test/scalar-services.git" },
          preset: "dockerfile",
          image: PINNED_IMAGE,
          exposed_port: 3000,
          services: "postgres",
          managed_service_bindings: "postgres",
        },
      ],
    });

    expect(result.valid).toBeFalse();
    expect(result.errors).toContain("templates[0].services must be an array");
    expect(result.errors).toContain(
      "templates[0].managed_service_bindings must be an object",
    );
  });

  test("rejects literal defaults for secret environment variables", () => {
    const result = validateNativeTemplateConfig({
      version: "2",
      templates: [
        {
          slug: "unsafe-secret",
          name: "Unsafe secret",
          kind: "service",
          version: "1.0.0",
          git: { url: "https://example.test/unsafe-secret.git" },
          preset: "dockerfile",
          image: PINNED_IMAGE,
          exposed_port: 3000,
          env_vars: [
            { name: "ADMIN_PASSWORD", default: "published-password" },
            { name: "SMTP_PASS", secret: true, default: "also-published" },
          ],
        },
      ],
    });

    expect(result.valid).toBeFalse();
    expect(result.errors).toContain(
      "templates[0].env_vars[0] is secret and cannot declare a literal default; use a secure generator or require user input",
    );
    expect(result.errors).toContain(
      "templates[0].env_vars[1] is secret and cannot declare a literal default; use a secure generator or require user input",
    );
  });

  test("rejects templates missing the server-required source and preset", () => {
    const result = validateNativeTemplateConfig({
      version: "2",
      templates: [
        {
          slug: "missing-source",
          name: "Missing source",
          kind: "starter",
        },
      ],
    });

    expect(result.valid).toBeFalse();
    expect(result.errors).toContain("templates[0].git is required");
    expect(result.errors).toContain("templates[0].preset is required");
  });

  test("rejects invalid Git URLs, presets, commands, and resource ranges", () => {
    const result = validateNativeTemplateConfig({
      version: "2",
      templates: [
        {
          slug: "invalid-runtime",
          name: "Invalid runtime",
          kind: "service",
          version: "1.0.0",
          git: { url: "local/path" },
          preset: "made-up",
          image: PINNED_IMAGE,
          command: [],
          exposed_port: 3000,
          resources: {
            cpu_request: 2_000_000,
            cpu_limit: 1_000_000,
            memory_request: 1024,
            memory_limit: 512,
          },
        },
      ],
    });

    expect(result.valid).toBeFalse();
    expect(result.errors).toContain(
      "templates[0].git.url must be an HTTP(S) or SSH Git URL",
    );
    expect(result.errors).toContain(
      'templates[0].preset "made-up" is not supported',
    );
    expect(result.errors).toContain(
      "templates[0].command must contain non-empty arguments",
    );
    expect(result.errors).toContain(
      "templates[0].resources.cpu_request must not exceed cpu_limit",
    );
    expect(result.errors).toContain(
      "templates[0].resources.memory_request must not exceed memory_limit",
    );
  });

  test("requires a release version for service templates", () => {
    const result = validateNativeTemplateConfig({
      version: "2",
      templates: [
        {
          slug: "unversioned-service",
          name: "Unversioned service",
          kind: "service",
          git: { url: "https://example.test/unversioned-service.git" },
          preset: "dockerfile",
          image: PINNED_IMAGE,
          exposed_port: 3000,
        },
      ],
    });

    expect(result.valid).toBeFalse();
    expect(result.errors).toContain(
      "templates[0].version is required for service templates",
    );
  });

  test("requires Semantic Versioning for service template releases", () => {
    const result = validateNativeTemplateConfig({
      version: "2",
      templates: [
        {
          slug: "invalid-version",
          name: "Invalid version",
          kind: "service",
          version: "next",
          git: { url: "https://example.test/invalid-version.git" },
          preset: "dockerfile",
          image: PINNED_IMAGE,
          exposed_port: 3000,
        },
      ],
    });

    expect(result.valid).toBeFalse();
    expect(result.errors).toContain(
      "templates[0].version must use Semantic Versioning",
    );
  });

  test("rejects leading zeroes in numeric prerelease identifiers", () => {
    const result = validateNativeTemplateConfig({
      version: "2",
      templates: [
        {
          slug: "invalid-prerelease",
          name: "Invalid prerelease",
          kind: "service",
          version: "1.0.0-01",
          git: { url: "https://example.test/invalid-prerelease.git" },
          preset: "dockerfile",
          image: PINNED_IMAGE,
          exposed_port: 3000,
        },
      ],
    });

    expect(result.valid).toBeFalse();
    expect(result.errors).toContain(
      "templates[0].version must use Semantic Versioning",
    );
  });
});
