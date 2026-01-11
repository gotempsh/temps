// Patterns that indicate a value should be masked
const SENSITIVE_PATTERNS = [
  /secret/i,
  /password/i,
  /token/i,
  /api[_-]?key/i,
  /auth/i,
  /credential/i,
  /private[_-]?key/i,
  /access[_-]?key/i,
  /sentry[_-]?dsn/i,
  /database[_-]?url/i,
  /connection[_-]?string/i,
  /jwt/i,
  /bearer/i,
  /postgres[_-]?url/i,
  /mysql[_-]?url/i,
  /redis[_-]?url/i,
]

/**
 * Check if a key name suggests the value should be masked
 */
export function shouldMaskValue(key: string): boolean {
  return SENSITIVE_PATTERNS.some((pattern) => pattern.test(key))
}

/**
 * Mask a value with bullet points, showing only last 4 characters
 * @param value - The value to mask
 * @returns Masked string with bullets and last 4 characters visible
 */
export function maskValue(value: string): string {
  if (!value || value.length <= 4) {
    return '••••••••'
  }
  const visiblePart = value.slice(-4)
  const maskedLength = Math.min(value.length - 4, 20)
  return '•'.repeat(maskedLength) + visiblePart
}
