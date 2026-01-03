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

  constructor(message: string, code?: string, status?: number) {
    super(message);
    this.name = 'BlobError';
    this.code = code;
    this.status = status;

    // Maintains proper stack trace for where error was thrown
    if (Error.captureStackTrace) {
      Error.captureStackTrace(this, BlobError);
    }
  }

  /**
   * Create a BlobError from an API response
   */
  static fromResponse(response: Response, body?: { error?: { message: string; code?: string } }): BlobError {
    const message = body?.error?.message || `Blob operation failed with status ${response.status}`;
    const code = body?.error?.code;
    return new BlobError(message, code, response.status);
  }

  /**
   * Create a BlobError for missing configuration
   */
  static missingConfig(field: string): BlobError {
    return new BlobError(
      `Missing required configuration: ${field}. Set ${field === 'apiUrl' ? 'TEMPS_API_URL' : 'TEMPS_TOKEN'} environment variable or pass it in config.`,
      'MISSING_CONFIG'
    );
  }

  /**
   * Create a BlobError for network errors
   */
  static networkError(originalError: Error): BlobError {
    return new BlobError(
      `Network error: ${originalError.message}`,
      'NETWORK_ERROR'
    );
  }

  /**
   * Create a BlobError for blob not found
   */
  static notFound(url: string): BlobError {
    return new BlobError(
      `Blob not found: ${url}`,
      'NOT_FOUND',
      404
    );
  }

  /**
   * Create a BlobError for invalid input
   */
  static invalidInput(message: string): BlobError {
    return new BlobError(
      message,
      'INVALID_INPUT',
      400
    );
  }
}
