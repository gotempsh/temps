/**
 * RFC 7807 Problem Details response format
 */
interface ProblemDetails {
  type?: string;
  title?: string;
  detail?: string;
  instance?: string;
  status?: number;
  [key: string]: unknown;
}

/**
 * Error thrown by KV operations
 */
export class KVError extends Error {
  /**
   * Error code from the API (or problem type URI)
   */
  public readonly code?: string;

  /**
   * HTTP status code if available
   */
  public readonly status?: number;

  /**
   * Problem title (short summary)
   */
  public readonly title?: string;

  /**
   * Problem detail (human-readable explanation)
   */
  public readonly detail?: string;

  constructor(
    message: string,
    options?: { code?: string; status?: number; title?: string; detail?: string }
  ) {
    super(message);
    this.name = 'KVError';
    this.code = options?.code;
    this.status = options?.status;
    this.title = options?.title;
    this.detail = options?.detail;

    // Maintains proper stack trace for where error was thrown
    if (Error.captureStackTrace) {
      Error.captureStackTrace(this, KVError);
    }
  }

  /**
   * Create a KVError from an API response
   * Handles both RFC 7807 Problem Details and legacy error formats
   */
  static fromResponse(
    response: Response,
    body?: ProblemDetails | { error?: { message: string; code?: string } }
  ): KVError {
    // Check for RFC 7807 Problem Details format (has title/detail fields)
    if (body && ('title' in body || 'detail' in body)) {
      const problemBody = body as ProblemDetails;
      const message = problemBody.detail || problemBody.title || `KV operation failed with status ${response.status}`;
      return new KVError(message, {
        code: problemBody.type,
        status: response.status,
        title: problemBody.title,
        detail: problemBody.detail,
      });
    }

    // Legacy error format
    const legacyBody = body as { error?: { message: string; code?: string } } | undefined;
    const message = legacyBody?.error?.message || `KV operation failed with status ${response.status}`;
    return new KVError(message, {
      code: legacyBody?.error?.code,
      status: response.status,
    });
  }

  /**
   * Create a KVError for missing configuration
   */
  static missingConfig(field: string): KVError {
    return new KVError(
      `Missing required configuration: ${field}. Set ${field === 'apiUrl' ? 'TEMPS_API_URL' : 'TEMPS_TOKEN'} environment variable or pass it in config.`,
      { code: 'MISSING_CONFIG' }
    );
  }

  /**
   * Create a KVError for network errors
   */
  static networkError(originalError: Error): KVError {
    return new KVError(
      `Network error: ${originalError.message}`,
      { code: 'NETWORK_ERROR' }
    );
  }
}
