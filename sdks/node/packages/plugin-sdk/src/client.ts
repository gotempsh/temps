// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ApiCall,
  ApiCallResult,
  ChannelMessage,
  ChannelRequest,
  ChannelResponse,
  DeploymentInfo,
  EnvironmentInfo,
  PlatformCallMap,
  PlatformCallResponse,
  PlatformMethod,
  PluginEvent,
  ProjectInfo,
} from "./types.js";
import {
  ChannelClosedError,
  ChannelTimeoutError,
  PlatformError,
  ProtocolMismatchError,
} from "./errors.js";

const DEFAULT_TIMEOUT_MS = 10_000;

export interface WsLike {
  on(event: "message", fn: (data: unknown) => void): void;
  on(event: "close", fn: () => void): void;
  on(event: "error", fn: (error: unknown) => void): void;
  send(data: string, callback?: (error?: Error) => void): void;
  close(): void;
}

interface PendingRequest {
  method: PlatformMethod;
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

export class TempsClient {
  readonly #ws: WsLike;
  readonly #pending = new Map<number, PendingRequest>();
  readonly #timeoutMs: number;
  #nextId = 1;
  #closed = false;
  #eventHandler?: (event: PluginEvent) => void | Promise<void>;

  constructor(ws: WsLike, options?: { timeoutMs?: number }) {
    this.#ws = ws;
    this.#timeoutMs = options?.timeoutMs ?? DEFAULT_TIMEOUT_MS;

    ws.on("message", (data) => this.#handleMessage(data));
    ws.on("close", () => this.#closePending());
    ws.on("error", (error) => {
      this.#closePending(error instanceof Error ? error : new ChannelClosedError());
    });
  }

  onEvent(handler: (event: PluginEvent) => void | Promise<void>): void {
    this.#eventHandler = handler;
  }

  getProject(projectId: number): Promise<ProjectInfo> {
    return this.call("get_project", { project_id: projectId });
  }

  listProjects(): Promise<ProjectInfo[]> {
    return this.call("list_projects", {});
  }

  getEnvironment(environmentId: number): Promise<EnvironmentInfo> {
    return this.call("get_environment", { environment_id: environmentId });
  }

  listEnvironments(projectId: number): Promise<EnvironmentInfo[]> {
    return this.call("list_environments", { project_id: projectId });
  }

  getDeployment(deploymentId: number): Promise<DeploymentInfo> {
    return this.call("get_deployment", { deployment_id: deploymentId });
  }

  getLastDeployment(
    projectId: number,
    environmentId?: number
  ): Promise<DeploymentInfo> {
    return this.call("get_last_deployment", compact({
      project_id: projectId,
      environment_id: environmentId,
    }));
  }

  listDeployments(
    projectId: number,
    options?: { environmentId?: number; limit?: number }
  ): Promise<DeploymentInfo[]> {
    return this.call("list_deployments", compact({
      project_id: projectId,
      environment_id: options?.environmentId,
      limit: options?.limit,
    }));
  }

  apiCall(call: ApiCall): Promise<ApiCallResult> {
    return this.call("api_call", call);
  }

  call<Method extends PlatformMethod>(
    method: Method,
    params: PlatformCallMap[Method]["params"]
  ): Promise<PlatformCallMap[Method]["result"]> {
    if (this.#closed) return Promise.reject(new ChannelClosedError());

    const id = this.#nextId++;
    const message: ChannelRequest = {
      type: "request",
      id,
      call: { method, params } as ChannelRequest["call"],
    };

    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new ChannelTimeoutError(method, this.#timeoutMs));
      }, this.#timeoutMs);

      this.#pending.set(id, {
        method,
        resolve: (value) => resolve(value as PlatformCallMap[Method]["result"]),
        reject,
        timer,
      });

      this.#ws.send(JSON.stringify(message), (error) => {
        if (!error) return;
        clearTimeout(timer);
        this.#pending.delete(id);
        reject(error);
      });
    });
  }

  close(): void {
    this.#closed = true;
    this.#ws.close();
    this.#closePending();
  }

  #handleMessage(data: unknown): void {
    let message: ChannelMessage;
    try {
      message = JSON.parse(normalizeMessage(data)) as ChannelMessage;
    } catch {
      return;
    }

    if (message.type === "response") {
      this.#handleResponse(message);
      return;
    }

    if (message.type === "event") {
      void Promise.resolve(this.#eventHandler?.(message.event)).catch(() => undefined);
    }
  }

  #handleResponse(message: ChannelResponse): void {
    const pending = this.#pending.get(message.id);
    if (!pending) return;

    clearTimeout(pending.timer);
    this.#pending.delete(message.id);

    if ("err" in message.outcome) {
      pending.reject(
        new PlatformError(message.outcome.err.code, message.outcome.err.message)
      );
      return;
    }

    const response: PlatformCallResponse = message.outcome.ok;
    if (response.method !== pending.method) {
      pending.reject(new ProtocolMismatchError(pending.method, response.method));
      return;
    }
    pending.resolve(response.result);
  }

  #closePending(reason: Error = new ChannelClosedError()): void {
    this.#closed = true;
    for (const pending of this.#pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(reason);
    }
    this.#pending.clear();
  }
}

function normalizeMessage(data: unknown): string {
  if (typeof data === "string") return data;
  if (data instanceof Uint8Array) return Buffer.from(data).toString("utf8");
  return String(data);
}

function compact<T extends Record<string, unknown>>(value: T): T {
  return Object.fromEntries(
    Object.entries(value).filter(([, entry]) => entry !== undefined)
  ) as T;
}
