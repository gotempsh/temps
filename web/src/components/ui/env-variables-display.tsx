import { useState } from 'react'
import { Eye, EyeOff } from 'lucide-react'
import { Button } from './button'
import { Badge } from './badge'
import { CopyButton } from './copy-button'
import { shouldMaskValue, maskValue } from '@/lib/masking'
import { cn } from '@/lib/utils'

interface EnvVariablesDisplayProps {
  variables: Record<string, string | number | boolean>
  className?: string
  showCopy?: boolean
  showMaskToggle?: boolean
  defaultMasked?: boolean
  maxHeight?: string
}

/**
 * Reusable component to display environment variables with optional masking
 * @param variables - Object of key-value pairs to display
 * @param className - Optional className for the container
 * @param showCopy - Show copy button (default: true)
 * @param showMaskToggle - Show mask/unmask toggle (default: true)
 * @param defaultMasked - Start with masking enabled (default: true)
 * @param maxHeight - Max height for the scrollable area (default: '10rem')
 */
export function EnvVariablesDisplay({
  variables,
  className,
  showCopy = true,
  showMaskToggle = true,
  defaultMasked = true,
  maxHeight = '10rem',
}: EnvVariablesDisplayProps) {
  const [isMasked, setIsMasked] = useState(defaultMasked)

  const entries = Object.entries(variables)
  const count = entries.length

  if (count === 0) {
    return (
      <div className="text-center py-8">
        <p className="text-sm text-muted-foreground">
          No environment variables available
        </p>
      </div>
    )
  }

  return (
    <div className={cn('space-y-3', className)}>
      {/* Header with count, copy button, and mask toggle */}
      <div className="flex items-center justify-between">
        <p className="text-xs text-muted-foreground">
          {count} environment variable{count !== 1 ? 's' : ''} available
        </p>
        <div className="flex items-center gap-2">
          {showCopy && (
            <CopyButton
              value={entries.map(([key, value]) => `${key}=${value}`).join('\n')}
            />
          )}
          {showMaskToggle && (
            <>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={() => setIsMasked(!isMasked)}
                className="h-7 px-2 text-xs"
              >
                {isMasked ? (
                  <>
                    <Eye className="h-3 w-3 mr-1" />
                    Unmask
                  </>
                ) : (
                  <>
                    <EyeOff className="h-3 w-3 mr-1" />
                    Mask
                  </>
                )}
              </Button>
              {isMasked && (
                <Badge variant="secondary" className="text-xs">
                  Masked
                </Badge>
              )}
            </>
          )}
        </div>
      </div>

      {/* Environment variables display */}
      <div className="relative">
        <pre
          className={cn(
            'bg-muted/30 border rounded-md p-3 text-xs font-mono',
            'overflow-y-auto overflow-x-auto',
            'whitespace-pre-wrap break-all'
          )}
          style={{ maxHeight }}
        >
          {entries.map(([key, value], index) => {
            const stringValue = String(value)
            const displayValue =
              isMasked && shouldMaskValue(key)
                ? maskValue(stringValue)
                : stringValue

            return (
              <span key={key}>
                <span className="text-primary font-medium">{key}</span>
                <span className="text-muted-foreground">=</span>
                <span className="text-foreground">{displayValue}</span>
                {index < entries.length - 1 ? '\n' : ''}
              </span>
            )
          })}
        </pre>
      </div>
    </div>
  )
}
