import { useEffect } from 'react'
import { cn } from '@/lib/utils'

interface KeyboardShortcutProps {
  shortcut: string
  onTrigger: () => void
  className?: string
  disabled?: boolean
  preventDefault?: boolean
  showOnMobile?: boolean
}

export function KeyboardShortcut({
  shortcut,
  onTrigger,
  className,
  disabled = false,
  preventDefault = true,
  showOnMobile = false,
}: KeyboardShortcutProps) {
  useEffect(() => {
    if (disabled) return

    const handleKeyPress = (e: KeyboardEvent) => {
      // Don't trigger if user is typing in an input or textarea
      if (
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement
      ) {
        return
      }

      // Don't trigger if modifier keys are pressed (Cmd/Ctrl) to avoid conflicts with browser shortcuts
      if (e.metaKey || e.ctrlKey) {
        return
      }

      // Check if the pressed key matches the shortcut (case-insensitive)
      if (e.key.toUpperCase() === shortcut.toUpperCase()) {
        if (preventDefault) {
          e.preventDefault()
        }
        onTrigger()
      }
    }

    window.addEventListener('keydown', handleKeyPress)
    return () => window.removeEventListener('keydown', handleKeyPress)
  }, [shortcut, onTrigger, disabled, preventDefault])

  return (
    <kbd
      className={cn(
        'ml-2 h-4 min-w-[1rem] items-center justify-center rounded border border-border bg-muted px-1 text-[0.625rem] font-medium text-muted-foreground',
        showOnMobile ? 'inline-flex' : 'hidden sm:inline-flex',
        className
      )}
    >
      {shortcut}
    </kbd>
  )
}
