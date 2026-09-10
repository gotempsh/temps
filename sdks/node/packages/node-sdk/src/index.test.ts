// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { beforeEach, describe, expect, expectTypeOf, it, vi } from "vitest";
import { TempsClient } from "./index";
import * as clientModule from "./client/client";
import * as sdk from "./client/sdk.gen";
import type {
  ListEmailDomainProjectsErrors,
  SendEmailData,
  SendEmailErrors,
} from "./client/types.gen";

vi.mock("./client/client", () => ({
  createClient: vi.fn(() => ({
    get: vi.fn(),
    post: vi.fn(),
    put: vi.fn(),
    delete: vi.fn(),
    patch: vi.fn(),
  })),
  createConfig: vi.fn((config) => config),
}));

vi.mock("./client/sdk.gen", () => ({
  getPlatformInfo: vi.fn().mockResolvedValue({ data: { version: "test" } }),
  listApiKeys: vi.fn().mockResolvedValue({ data: [] }),
  getProjects: vi.fn().mockResolvedValue({ data: { projects: [] } }),
  getBackup: vi.fn().mockResolvedValue({ data: { id: 17 } }),
  sendEmail: vi
    .fn()
    .mockResolvedValue({ data: { id: "email-1", status: "sent" } }),
  listEmailDomainProjects: vi.fn().mockResolvedValue({ data: [] }),
  authorizeEmailDomainProject: vi.fn().mockResolvedValue({ data: undefined }),
  revokeEmailDomainProject: vi.fn().mockResolvedValue({ data: undefined }),
}));

describe("TempsClient", () => {
  let client: TempsClient;

  beforeEach(() => {
    vi.clearAllMocks();
    client = new TempsClient({
      baseUrl: "https://api.test.com",
      apiKey: "test-api-key",
    });
  });

  it("creates one authenticated client shared by every namespace", () => {
    expect(clientModule.createConfig).toHaveBeenCalledWith({
      baseUrl: "https://api.test.com",
      headers: { Authorization: "Bearer test-api-key" },
    });
    expect(client.rawClient).toBeDefined();

    for (const namespace of [
      "apiKeys",
      "analytics",
      "auditLogs",
      "authentication",
      "backups",
      "crons",
      "deployments",
      "dns",
      "domains",
      "email",
      "externalServices",
      "files",
      "funnels",
      "git",
      "loadBalancer",
      "monitoring",
      "notifications",
      "performance",
      "platform",
      "projects",
      "proxyLogs",
      "repositories",
      "sessionReplay",
      "settings",
      "users",
    ]) {
      expect(client).toHaveProperty(namespace);
    }
  });

  it("omits the authorization header when no API key is configured", () => {
    vi.clearAllMocks();
    new TempsClient({ baseUrl: "https://api.test.com" });

    expect(clientModule.createConfig).toHaveBeenCalledWith({
      baseUrl: "https://api.test.com",
      headers: undefined,
    });
  });

  it("routes email-domain project reads and writes through the configured client", async () => {
    const listOptions = { path: { id: 7 } };
    const writeOptions = { path: { id: 7, project_id: 42 } };

    await client.email.listAuthorizedProjects(listOptions);
    await client.email.authorizeProject(writeOptions);
    await client.email.revokeProject(writeOptions);

    expect(sdk.listEmailDomainProjects).toHaveBeenCalledWith({
      ...listOptions,
      client: client.rawClient,
    });
    expect(sdk.authorizeEmailDomainProject).toHaveBeenCalledWith({
      ...writeOptions,
      client: client.rawClient,
    });
    expect(sdk.revokeEmailDomainProject).toHaveBeenCalledWith({
      ...writeOptions,
      client: client.rawClient,
    });
  });

  it("forwards representative namespace calls through the shared client", async () => {
    await client.platform.getInfo();
    await client.apiKeys.list();
    await client.projects.list({ query: { page: 2, per_page: 25 } });
    await client.backups.get({ path: { id: "backup-17" } });

    expect(sdk.getPlatformInfo).toHaveBeenCalledWith({
      client: client.rawClient,
    });
    expect(sdk.listApiKeys).toHaveBeenCalledWith({ client: client.rawClient });
    expect(sdk.getProjects).toHaveBeenCalledWith({
      query: { page: 2, per_page: 25 },
      client: client.rawClient,
    });
    expect(sdk.getBackup).toHaveBeenCalledWith({
      path: { id: "backup-17" },
      client: client.rawClient,
    });
  });

  it("forwards deployment idempotency headers when sending email", async () => {
    const options = {
      body: {
        from: "sender@example.test",
        to: ["recipient@example.test"],
        subject: "Status update",
        text: "All systems operational",
      },
      headers: { "Idempotency-Key": "notification:status-42" },
    };

    await client.email.send(options);

    expect(sdk.sendEmail).toHaveBeenCalledWith({
      ...options,
      client: client.rawClient,
    });
  });

  it("keeps email idempotency and authorization errors in the generated contract", () => {
    expectTypeOf<NonNullable<SendEmailData["headers"]>["Idempotency-Key"]>()
      .toEqualTypeOf<string | null | undefined>();
    expectTypeOf<keyof SendEmailErrors>().toEqualTypeOf<400 | 401 | 403 | 409 | 500>();
    expectTypeOf<keyof ListEmailDomainProjectsErrors>()
      .toEqualTypeOf<401 | 403 | 404 | 500>();
  });

  it("propagates authorization failures to the caller", async () => {
    vi.mocked(sdk.authorizeEmailDomainProject).mockRejectedValueOnce(
      new Error("forbidden"),
    );

    await expect(
      client.email.authorizeProject({ path: { id: 7, project_id: 42 } }),
    ).rejects.toThrow("forbidden");
  });
});
