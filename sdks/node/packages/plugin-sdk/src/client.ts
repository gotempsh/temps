// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * TempsClient -- typed async client for querying platform data over
 * the WebSocket data channel.
 *
 * Mirrors: crates/temps-plugin-sdk/src/client.rs
 *
 * The client maintains a monotonically increasing request ID and a map
 * of pending requests. Each request sends a `ChannelRequest` frame over
 * the WebSocket and waits for a matching `ChannelResponse` by ID.
 */

import type {
  ChannelMessage,
  ChannelRequest,
  ChannelResponse,
  ProjectInfo,
  EnvironmentInfo,
  DeploymentInfo,
  PluginEvent,
} from "./types.js";
import {
  ChannelClosedError,
  ChannelTimeoutError,
  PlatformError,
} from "./errors.js";

const DEFAULT_TIMEOUT_MS = 10_000;

/**
 * Minimal WebSocket-like interface accepted by TempsClient.
 * Compatible with both the `ws` package and the Bun WebSocket adapter.
 */
export interface WsLike {
  on(event: string, fn: (...args: unknown[]) => void): void;
  send(data: string, cb?: (err?: Error) => void): void;
  close(): void;
}

interface PendingRequest {
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

export class TempsClient {
  private ws: WsLike;
  private nextId = 1;
  private pending = new Map<number, PendingRequest>();
  private closed = false;
  private eventHandler?: (event: PluginEvent) => void;
  private timeoutMs: number;

  constructor(ws: WsLike, options?: { timeoutMs?: number }) {
    this.ws = ws;
    this.timeoutMs = options?.timeoutMs ?? DEFAULT_TIMEOUT_MS;

    this.ws.on("message", (data: unknown) => {
      this.handleMessage(data);
    });

    this.ws.on("close", () => {
      this.closed = true;
      // Reject all pending requests
      for (const [id, pending] of this.pending) {
        clearTimeout(pending.timer);
        pending.reject(new ChannelClosedError());
        this.pending.delete(id);
      }
    });

    this.ws.on("error", (err: unknown) => {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("[temps-plugin] WebSocket error:", msg);
    });
  }

  /**
   * Register a handler for platform events pushed over the channel.
   */
  onEvent(handler: (event: PluginEvent) => void): void {
    this.eventHandler = handler;
  }

  // -------------------------------------------------------------------------
  // Platform Data Queries
  // -------------------------------------------------------------------------

  async getProject(projectId: number): Promise<ProjectInfo> {
    return this.request<ProjectInfo>("get_project", { project_id: projectId });
  }

  async listProjects(limit?: number): Promise<ProjectInfo[]> {
    return this.request<ProjectInfo[]>("list_projects", { limit });
  }

  async getEnvironment(environmentId: number): Promise<EnvironmentInfo> {
    return this.request<EnvironmentInfo>("get_environment", {
      environment_id: environmentId,
    });
  }

  async listEnvironments(projectId: number): Promise<EnvironmentInfo[]> {
    return this.request<EnvironmentInfo[]>("list_environments", {
      project_id: projectId,
    });
  }

  async getDeployment(deploymentId: number): Promise<DeploymentInfo> {
    return this.request<DeploymentInfo>("get_deployment", {
      deployment_id: deploymentId,
    });
  }

  async getLastDeployment(
    projectId: number,
    environmentId?: number
  ): Promise<DeploymentInfo> {
    return this.request<DeploymentInfo>("get_last_deployment", {
      project_id: projectId,
      environment_id: environmentId,
    });
  }

  async listDeployments(
    projectId: number,
    options?: { environmentId?: number; limit?: number }
  ): Promise<DeploymentInfo[]> {
    return this.request<DeploymentInfo[]>("list_deployments", {
      project_id: projectId,
      environment_id: options?.environmentId,
      limit: options?.limit,
    });
  }

  // -------------------------------------------------------------------------
  // Low-level request/response
  // -------------------------------------------------------------------------

  /**
   * Send a typed channel request and wait for the response.
   */
  private request<T>(
    method: string,
    params: Record<string, unknown>
  ): Promise<T> {
    if (this.closed) {
      return Promise.reject(new ChannelClosedError());
    }

    const id = this.nextId++;

    // Filter out undefined values from params
    const cleanParams: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(params)) {
      if (value !== undefined) {
        cleanParams[key] = value;
      }
    }

    const msg: ChannelRequest = {
      type: "request",
      id,
      method,
      params: cleanParams,
    };

    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new ChannelTimeoutError(method, this.timeoutMs));
      }, this.timeoutMs);

      this.pending.set(id, {
        resolve: resolve as (value: unknown) => void,
        reject,
        timer,
      });

      this.ws.send(JSON.stringify(msg), (err) => {
        if (err) {
          clearTimeout(timer);
          this.pending.delete(id);
          reject(err);
        }
      });
    });
  }

  private handleMessage(data: unknown): void {
    let msg: ChannelMessage;
    try {
      const str = typeof data === "string" ? data : String(data);
      msg = JSON.parse(str) as ChannelMessage;
    } catch {
      console.error("[temps-plugin] Failed to parse channel message");
      return;
    }

    switch (msg.type) {
      case "response":
        this.handleResponse(msg as ChannelResponse);
        break;
      case "event":
        if (this.eventHandler) {
          this.eventHandler(msg.event);
        }
        break;
      case "request":
        // Plugins don't handle inbound requests from the host (yet)
        console.warn(
          `[temps-plugin] Received unexpected request: ${msg.method}`
        );
        break;
    }
  }

  private handleResponse(msg: ChannelResponse): void {
    const pending = this.pending.get(msg.id);
    if (!pending) {
      console.warn(
        `[temps-plugin] Received response for unknown request ID: ${msg.id}`
      );
      return;
    }

    clearTimeout(pending.timer);
    this.pending.delete(msg.id);

    if (msg.error) {
      pending.reject(new PlatformError(msg.error.code, msg.error.message));
    } else {
      pending.resolve(msg.result);
    }
  }

  /**
   * Close the WebSocket connection.
   */
  close(): void {
    this.closed = true;
    this.ws.close();
  }
}
