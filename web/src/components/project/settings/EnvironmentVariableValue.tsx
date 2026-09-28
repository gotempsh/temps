// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useLayoutEffect, useRef, useState } from 'react'
import { Expand } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'

/** Only mount after an explicit, successful credential reveal. */
export function EnvironmentVariableValue({
  name,
  value,
}: {
  name: string
  value: string
}) {
  const valueRef = useRef<HTMLPreElement>(null)
  const previewRef = useRef<HTMLSpanElement>(null)
  const [isClipped, setIsClipped] = useState(false)
  useLayoutEffect(() => {
    const preview = previewRef.current
    if (!preview) return
    const measure = () =>
      setIsClipped(preview.scrollWidth > preview.clientWidth)
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(preview)
    return () => observer.disconnect()
  }, [value])
  const needsExpansion = isClipped || /[\r\n]/.test(value)
  return (
    <Dialog>
      <span className="inline-flex min-w-0 max-w-60 items-center gap-2 font-mono text-sm">
        <span ref={previewRef} className="min-w-0 truncate">
          {value === '' ? '(empty value)' : value}
        </span>
        {needsExpansion && (
          <DialogTrigger asChild>
            <Button
              variant="ghost"
              size="sm"
              className="shrink-0 px-1"
              aria-label={`View full ${name} value`}
            >
              <Expand className="size-3.5" aria-hidden="true" />
            </Button>
          </DialogTrigger>
        )}
      </span>
      <DialogContent
        onOpenAutoFocus={(event) => {
          event.preventDefault()
          valueRef.current?.focus()
        }}
      >
        <DialogHeader>
          <DialogTitle className="break-all">{name}</DialogTitle>
          <DialogDescription>
            Full revealed value. Line breaks are preserved.
          </DialogDescription>
        </DialogHeader>
        <pre
          ref={valueRef}
          tabIndex={0}
          aria-label="Full value"
          className="max-h-[60vh] overflow-y-auto whitespace-pre-wrap break-all rounded-md border bg-muted/30 p-3 font-mono text-sm"
        >
          {value === '' ? '(empty value)' : value}
        </pre>
        <CopyButton value={value} label={`Copy ${name} value`} />
      </DialogContent>
    </Dialog>
  )
}
