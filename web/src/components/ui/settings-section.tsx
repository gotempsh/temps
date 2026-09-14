// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect, useRef, useState, type ReactNode } from 'react'
import { ChevronDown, type LucideIcon } from 'lucide-react'
import { cn } from '@/lib/utils'

/** Native disclosures retain mounted form fields and their unsaved values. */
export function SettingsSection({
  title,
  icon: Icon,
  children,
  defaultOpen = false,
  hasError = false,
  description,
  className,
}: {
  title: string
  icon: LucideIcon
  children: ReactNode
  defaultOpen?: boolean
  hasError?: boolean
  description?: string
  className?: string
}) {
  const [open, setOpen] = useState(defaultOpen || hasError)
  const detailsRef = useRef<HTMLDetailsElement>(null)
  const contentRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const content = contentRef.current
    if (!content || typeof MutationObserver === 'undefined') return

    const revealInvalidField = () => {
      if (!content.querySelector('[aria-invalid="true"]')) return
      // Update the native disclosure immediately. A close `toggle` event may
      // still be queued when validation appears, so state alone can lose the
      // race and leave the invalid field hidden.
      if (detailsRef.current) detailsRef.current.open = true
      setOpen(true)
    }
    revealInvalidField()
    const observer = new MutationObserver(revealInvalidField)
    observer.observe(content, {
      attributes: true,
      attributeFilter: ['aria-invalid'],
      childList: true,
      subtree: true,
    })
    return () => observer.disconnect()
  }, [])

  return (
    <details
      ref={detailsRef}
      open={open || hasError}
      onToggle={(event) => {
        const hasInvalidField = Boolean(
          contentRef.current?.querySelector('[aria-invalid="true"]')
        )
        if (hasInvalidField && !event.currentTarget.open) {
          event.currentTarget.open = true
        }
        setOpen(event.currentTarget.open || hasInvalidField)
      }}
      className={cn(
        'group/settings min-w-0 rounded-lg border bg-background',
        className
      )}
      onInvalidCapture={() => {
        if (detailsRef.current) detailsRef.current.open = true
        setOpen(true)
      }}
    >
      <summary className="flex cursor-pointer list-none items-center gap-3 rounded-lg px-4 py-3 hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
        <Icon
          className="size-4 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
        <span className="min-w-0 flex-1">
          <span className="block text-sm font-medium">{title}</span>
          {description && (
            <span className="block text-xs text-muted-foreground">
              {description}
            </span>
          )}
        </span>
        <ChevronDown
          className="size-4 shrink-0 text-muted-foreground transition-transform group-open/settings:rotate-180"
          aria-hidden="true"
        />
      </summary>
      <div ref={contentRef} className="min-w-0 border-t p-4 sm:p-5">
        {children}
      </div>
    </details>
  )
}
