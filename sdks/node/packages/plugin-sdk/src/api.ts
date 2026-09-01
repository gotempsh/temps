// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { AuthenticatedCaller } from "./auth.js";
import type { TempsClient } from "./client.js";
import { ActorTokenRequiredError, CallBodyTooLargeError, PluginSdkError } from "./errors.js";
import type {
  ApiCall,
  CallBody,
  HttpMethod,
  MultipartPart,
} from "./types.js";
import { MAX_CALL_BODY_BYTES } from "./types.js";

export interface PlatformApiResponse<T> {
  status: number;
  body: T;
  ok: boolean;
}

export interface PlatformApiRequestOptions {
  query?: string | URLSearchParams;
  body?: CallBody;
}

/** Caller-scoped access to the platform's own HTTP API over the plugin channel. */
export class PlatformApi {
  readonly #client: TempsClient;
  readonly #actorToken: string;

  constructor(client: TempsClient, caller: AuthenticatedCaller) {
    const actorToken = caller.actorToken();
    if (!actorToken) throw new ActorTokenRequiredError();
    this.#client = client;
    this.#actorToken = actorToken;
  }

  get<T>(path: string, query?: string | URLSearchParams): Promise<PlatformApiResponse<T>> {
    return this.request("GET", path, { query });
  }

  post<T>(path: string, body?: CallBody): Promise<PlatformApiResponse<T>> {
    return this.request("POST", path, { body });
  }

  put<T>(path: string, body?: CallBody): Promise<PlatformApiResponse<T>> {
    return this.request("PUT", path, { body });
  }

  patch<T>(path: string, body?: CallBody): Promise<PlatformApiResponse<T>> {
    return this.request("PATCH", path, { body });
  }

  delete<T>(path: string, body?: CallBody): Promise<PlatformApiResponse<T>> {
    return this.request("DELETE", path, { body });
  }

  async request<T>(
    method: HttpMethod,
    path: string,
    options: PlatformApiRequestOptions = {}
  ): Promise<PlatformApiResponse<T>> {
    if (!path.startsWith("/") || path.startsWith("/api/") || path.includes("..")) {
      throw new PluginSdkError(
        `Platform API path must be absolute below /api and cannot contain traversal segments: ${path}`
      );
    }

    if (options.body) assertBodySize(options.body);

    const call: ApiCall = {
      method,
      path,
      actor: this.#actorToken,
      ...(options.query === undefined
        ? {}
        : { query: options.query.toString().replace(/^\?/, "") }),
      ...(options.body === undefined ? {} : { body: options.body }),
    };

    const result = await this.#client.apiCall(call);
    let body: T;
    try {
      body = (result.body === "" ? undefined : JSON.parse(result.body)) as T;
    } catch (error) {
      throw new PluginSdkError(
        `Failed to decode ${result.body.length}-byte platform API response as JSON: ${error instanceof Error ? error.message : String(error)}`
      );
    }

    return {
      status: result.status,
      body,
      ok: result.status >= 200 && result.status < 300,
    };
  }

  static json(value: unknown): CallBody {
    return { kind: "json", body: JSON.stringify(value) };
  }

  static multipart(parts: MultipartPart[]): CallBody {
    return { kind: "multipart", body: parts };
  }

  static textPart(name: string, value: string): MultipartPart {
    return { name, content: { kind: "text", value } };
  }

  static binaryPart(options: {
    name: string;
    bytes: Uint8Array;
    filename?: string;
    contentType?: string;
  }): MultipartPart {
    return {
      name: options.name,
      ...(options.filename === undefined ? {} : { filename: options.filename }),
      ...(options.contentType === undefined
        ? {}
        : { content_type: options.contentType }),
      content: {
        kind: "binary",
        value: Buffer.from(options.bytes).toString("base64"),
      },
    };
  }
}

function assertBodySize(body: CallBody): void {
  const bytes =
    body.kind === "json"
      ? Buffer.byteLength(body.body)
      : body.body.reduce((total, part) => {
          const contentBytes =
            part.content.kind === "text"
              ? Buffer.byteLength(part.content.value)
              : Buffer.from(part.content.value, "base64").byteLength;
          return total + contentBytes;
        }, 0);

  if (bytes > MAX_CALL_BODY_BYTES) {
    throw new CallBodyTooLargeError(bytes, MAX_CALL_BODY_BYTES);
  }
}
