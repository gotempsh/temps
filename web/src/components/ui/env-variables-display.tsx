// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { HighlightedCode } from '@/components/ui/code-block'

import { useState } from 'react'
import { Eye, EyeOff, Loader2 } from 'lucide-react'
import { Button } from './button'
import { Badge } from './badge'
import { CopyButton } from './copy-button'
import { shouldMaskValue, maskValue } from '@/lib/masking'
import { cn } from '@/lib/utils'
import { getErrorMessage } from '@/utils/errorHandling'

interface EnvVariablesDisplayProps {
  variables: Record<string, string | number | boolean>
  className?: string
  showCopy?: boolean
  showMaskToggle?: boolean
  defaultMasked?: boolean
  maxHeight?: string
  /**
   * The values in `variables` were masked by the server; this fetches them
   * in plaintext (an audited reveal). The toggle then reveals and hides the
   * real values instead of re-masking placeholders, and nothing is offered
   * for copying until they are revealed.
   */
  reveal?: () => Promise<Record<string, string>>
}

/**
 * Reusable component to display environment variables with optional masking
 * @param variables - Object of key-value pairs to display
 * @param className - Optional className for the container
 * @param showCopy - Show copy button (default: true)
 * @param showMaskToggle - Show mask/unmask toggle (default: true)
 * @param defaultMasked - Start with masking enabled (default: true)
 * @param maxHeight - Max height for the scrollable area (default: '10rem')
 * @param reveal - Fetch plaintext values when `variables` are server-masked
 */
export function EnvVariablesDisplay({
  variables,
  className,
  showCopy = true,
  showMaskToggle = true,
  defaultMasked = true,
  maxHeight = '10rem',
  reveal,
}: EnvVariablesDisplayProps) {
  const [isMasked, setIsMasked] = useState(defaultMasked)
  const [revealed, setRevealed] = useState<Record<string, string> | null>(null)
  const [revealing, setRevealing] = useState(false)
  const [revealError, setRevealError] = useState<string | null>(null)

  const serverMasked = Boolean(reveal)
  const masked = serverMasked ? revealed === null : isMasked
  const entries = Object.entries(revealed ?? variables)
  const count = entries.length

  const toggle = async () => {
    if (!reveal) {
      setIsMasked(!isMasked)
      return
    }
    if (revealed) {
      setRevealed(null)
      return
    }
    setRevealError(null)
    setRevealing(true)
    try {
      setRevealed(await reveal())
    } catch (err) {
      setRevealError(getErrorMessage(err, 'The values could not be revealed.'))
    } finally {
      setRevealing(false)
    }
  }

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
          {showCopy && !(serverMasked && masked) && (
            <CopyButton
              value={entries
                .map(([key, value]) => `${key}=${value}`)
                .join('\n')}
            />
          )}
          {showMaskToggle && (
            <>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={() => void toggle()}
                disabled={revealing}
                className="h-7 px-2 text-xs"
              >
                {revealing ? (
                  <>
                    <Loader2 className="h-3 w-3 mr-1 animate-spin" />
                    Revealing…
                  </>
                ) : masked ? (
                  <>
                    <Eye className="h-3 w-3 mr-1" />
                    {serverMasked ? 'Reveal values' : 'Unmask'}
                  </>
                ) : (
                  <>
                    <EyeOff className="h-3 w-3 mr-1" />
                    {serverMasked ? 'Hide values' : 'Mask'}
                  </>
                )}
              </Button>
              {masked && (
                <Badge variant="secondary" className="text-xs">
                  Masked
                </Badge>
              )}
            </>
          )}
        </div>
      </div>

      {revealError && (
        <p role="alert" className="text-xs text-destructive">
          {revealError}
        </p>
      )}

      {/* Environment variables display */}
      <div className="relative">
        <pre
          tabIndex={0}
          className={cn(
            'bg-muted/30 border rounded-md p-3 text-xs font-mono',
            'overflow-y-auto overflow-x-auto',
            'whitespace-pre-wrap break-all'
          )}
          style={{ maxHeight }}
        >
          <HighlightedCode
            language="bash"
            code={entries
              .map(([key, value]) => {
                // Server-masked values are already placeholders; revealed
                // ones were asked for explicitly and are shown as they are.
                const displayValue =
                  !serverMasked && isMasked && shouldMaskValue(key)
                    ? maskValue(String(value))
                    : String(value)
                return `${key}=${displayValue}`
              })
              .join('\n')}
          />
        </pre>
      </div>
    </div>
  )
}
