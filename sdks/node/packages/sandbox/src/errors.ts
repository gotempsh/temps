// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The API returns RFC 7807 Problem Details on every error path
// (see `temps-core::problemdetails`). Mirror that shape here so
// callers can `if (err instanceof SandboxError)` and read a useful
// message without parsing JSON by hand.

interface ProblemDetails {
  type?: string;
  title?: string;
  detail?: string;
  instance?: string;
  status?: number;
}

export class SandboxError extends Error {
  public readonly code?: string;
  public readonly status?: number;
  public readonly title?: string;
  public readonly detail?: string;

  constructor(
    message: string,
    options?: {
      code?: string;
      status?: number;
      title?: string;
      detail?: string;
    }
  ) {
    super(message);
    this.name = 'SandboxError';
    this.code = options?.code;
    this.status = options?.status;
    this.title = options?.title;
    this.detail = options?.detail;

    if (Error.captureStackTrace) {
      Error.captureStackTrace(this, SandboxError);
    }
  }

  static fromResponse(
    response: Response,
    body?: ProblemDetails | { error?: { message: string; code?: string } }
  ): SandboxError {
    if (body && typeof body === 'object' && ('title' in body || 'detail' in body)) {
      const pd = body as ProblemDetails;
      const message =
        pd.detail ??
        pd.title ??
        `Sandbox request failed with status ${response.status}`;
      return new SandboxError(message, {
        code: pd.type,
        status: response.status,
        title: pd.title,
        detail: pd.detail,
      });
    }
    const legacy = body as { error?: { message: string; code?: string } } | undefined;
    return new SandboxError(
      legacy?.error?.message ??
        `Sandbox request failed with status ${response.status}`,
      { code: legacy?.error?.code, status: response.status }
    );
  }

  static missingConfig(field: 'apiUrl' | 'apiToken'): SandboxError {
    const envVar = field === 'apiUrl' ? 'TEMPS_API_URL' : 'TEMPS_API_TOKEN';
    return new SandboxError(
      `Missing required configuration: ${field}. Set ${envVar} or pass it in the config.`,
      { code: 'MISSING_CONFIG' }
    );
  }

  static networkError(originalError: Error): SandboxError {
    return new SandboxError(`Network error: ${originalError.message}`, {
      code: 'NETWORK_ERROR',
    });
  }
}
