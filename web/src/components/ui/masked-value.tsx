import { Eye, EyeOff } from 'lucide-react'
import { useState } from 'react'
import { Button } from './button'

interface MaskedValueProps {
  value: string
  className?: string
}

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
]

// Check if a key name suggests the value should be masked
export function shouldMaskValue(key: string): boolean {
  return SENSITIVE_PATTERNS.some((pattern) => pattern.test(key))
}

// Mask a value with asterisks, showing only last 4 characters
export function maskValue(value: string): string {
  if (!value || value.length <= 4) {
    return '••••••••'
  }
  const visiblePart = value.slice(-4)
  const maskedLength = Math.min(value.length - 4, 20)
  return '•'.repeat(maskedLength) + visiblePart
}

export function MaskedValue({ value, className = '' }: MaskedValueProps) {
  const [isVisible, setIsVisible] = useState(false)

  return (
    <div className={`flex items-center gap-2 ${className}`}>
      <code className="px-2 py-1 bg-muted rounded text-xs break-all flex-1 font-mono">
        {isVisible ? value : maskValue(value)}
      </code>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="h-7 w-7 flex-shrink-0"
        onClick={() => setIsVisible(!isVisible)}
      >
        {isVisible ? (
          <EyeOff className="h-3.5 w-3.5" />
        ) : (
          <Eye className="h-3.5 w-3.5" />
        )}
      </Button>
    </div>
  )
}
