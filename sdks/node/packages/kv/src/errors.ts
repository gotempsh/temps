/**
 * Error thrown by KV operations
 */
export class KVError extends Error {
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
    this.name = 'KVError';
    this.code = code;
    this.status = status;

    // Maintains proper stack trace for where error was thrown
    if (Error.captureStackTrace) {
      Error.captureStackTrace(this, KVError);
    }
  }

  /**
   * Create a KVError from an API response
   */
  static fromResponse(response: Response, body?: { error?: { message: string; code?: string } }): KVError {
    const message = body?.error?.message || `KV operation failed with status ${response.status}`;
    const code = body?.error?.code;
    return new KVError(message, code, response.status);
  }

  /**
   * Create a KVError for missing configuration
   */
  static missingConfig(field: string): KVError {
    return new KVError(
      `Missing required configuration: ${field}. Set ${field === 'apiUrl' ? 'TEMPS_API_URL' : 'TEMPS_TOKEN'} environment variable or pass it in config.`,
      'MISSING_CONFIG'
    );
  }

  /**
   * Create a KVError for network errors
   */
  static networkError(originalError: Error): KVError {
    return new KVError(
      `Network error: ${originalError.message}`,
      'NETWORK_ERROR'
    );
  }
}
