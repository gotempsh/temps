'use client'

import * as React from 'react'
import { CheckIcon, CopyIcon, XIcon } from 'lucide-react'
import { toast } from 'sonner'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'

import { cn } from '@/lib/utils'
import { ButtonProps } from '@/components/ui/button'
import { writeToClipboard } from '@/lib/clipboard'

interface CopyButtonProps extends ButtonProps {
  value: string
  children?: React.ReactNode
  minimal?: boolean
  /** Overrides the tooltip text and the accessible label. */
  label?: string
}

/**
 * Copy `value` to the clipboard, or throw explaining why not.
 *
 * `navigator.clipboard` only exists in a secure context, which is not an edge
 * case here: a self-hosted instance reached over plain http on a LAN address
 * does not have it. So this falls back to a selection-based copy when the API
 * is missing, and lets the caller report a genuine failure — a checkmark shown
 * over an empty clipboard is only discovered later, by pasting the wrong thing
 * somewhere it matters.
 */
export function CopyButton({
  value,
  className,
  children,
  minimal = false,
  label = 'Copy to clipboard',
  ...props
}: CopyButtonProps) {
  const [state, setState] = React.useState<'idle' | 'copied' | 'failed'>('idle')

  React.useEffect(() => {
    if (state === 'idle') return
    const timeout = setTimeout(() => setState('idle'), 2000)
    return () => clearTimeout(timeout)
  }, [state])

  const handleCopy = async () => {
    try {
      await writeToClipboard(value)
      setState('copied')
    } catch (e) {
      // Both the icon and a toast: the icon alone is easy to miss, and the
      // reason (almost always an insecure origin) is not guessable.
      setState('failed')
      toast.error("Couldn't copy to clipboard", {
        description:
          e instanceof Error && e.message
            ? e.message
            : 'Your browser blocked the copy. Select the text and copy it manually.',
      })
    }
  }

  const Icon =
    state === 'copied' ? CheckIcon : state === 'failed' ? XIcon : CopyIcon

  const buttonContent = (
    <button
      type="button"
      tabIndex={0}
      aria-label={label}
      className={cn(
        'inline-flex items-center justify-center gap-2 text-sm font-medium transition-colors hover:bg-accent hover:text-accent-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:pointer-events-none disabled:opacity-50',
        state === 'failed' && 'text-destructive',
        className
      )}
      onClick={() => void handleCopy()}
      {...props}
    >
      {children}
      <Icon className="h-4 w-4" />
    </button>
  )

  if (minimal) {
    return buttonContent
  }

  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>{buttonContent}</TooltipTrigger>
        <TooltipContent>
          <p>
            {state === 'copied'
              ? 'Copied'
              : state === 'failed'
                ? 'Copy failed'
                : label}
          </p>
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  )
}
