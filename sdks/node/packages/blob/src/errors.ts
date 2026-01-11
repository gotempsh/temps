/**
 * RFC 7807 Problem Details format
 */
interface ProblemDetails {
  type?: string;
  title?: string;
  detail?: string;
  instance?: string;
  status?: number;
}

/**
 * Legacy error format
 */
interface LegacyError {
  error?: {
    message: string;
    code?: string;
  };
}

/**
 * Error thrown by Blob operations
 */
export class BlobError extends Error {
  /**
   * Error code from the API
   */
  public readonly code?: string;

  /**
   * HTTP status code if available
   */
  public readonly status?: number;

  /**
   * RFC 7807 Problem Details title
   */
  public readonly title?: string;

  /**
   * RFC 7807 Problem Details detail
   */
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
    this.name = 'BlobError';
    this.code = options?.code;
    this.status = options?.status;
    this.title = options?.title;
    this.detail = options?.detail;

    // Maintains proper stack trace for where error was thrown
    if (Error.captureStackTrace) {
      Error.captureStackTrace(this, BlobError);
    }
  }

  /**
   * Create a BlobError from an API response
   * Supports both RFC 7807 Problem Details and legacy error formats
   */
  static fromResponse(
    response: Response,
    body?: ProblemDetails | LegacyError
  ): BlobError {
    // Handle RFC 7807 Problem Details format (title, detail)
    if (body && ('title' in body || 'detail' in body)) {
      const problemBody = body as ProblemDetails;
      const message =
        problemBody.detail ||
        problemBody.title ||
        `Blob operation failed with status ${response.status}`;
      return new BlobError(message, {
        code: problemBody.type,
        status: response.status,
        title: problemBody.title,
        detail: problemBody.detail,
      });
    }

    // Handle legacy error format (error.message, error.code)
    const legacyBody = body as LegacyError | undefined;
    const message =
      legacyBody?.error?.message ||
      `Blob operation failed with status ${response.status}`;
    const code = legacyBody?.error?.code;
    return new BlobError(message, { code, status: response.status });
  }

  /**
   * Create a BlobError for missing configuration
   */
  static missingConfig(field: string): BlobError {
    return new BlobError(
      `Missing required configuration: ${field}. Set ${field === 'apiUrl' ? 'TEMPS_API_URL' : 'TEMPS_TOKEN'} environment variable or pass it in config.`,
      { code: 'MISSING_CONFIG' }
    );
  }

  /**
   * Create a BlobError for network errors
   */
  static networkError(originalError: Error): BlobError {
    return new BlobError(`Network error: ${originalError.message}`, {
      code: 'NETWORK_ERROR',
    });
  }

  /**
   * Create a BlobError for blob not found
   */
  static notFound(url: string): BlobError {
    return new BlobError(`Blob not found: ${url}`, {
      code: 'NOT_FOUND',
      status: 404,
    });
  }

  /**
   * Create a BlobError for invalid input
   */
  static invalidInput(message: string): BlobError {
    return new BlobError(message, {
      code: 'INVALID_INPUT',
      status: 400,
    });
  }
}
