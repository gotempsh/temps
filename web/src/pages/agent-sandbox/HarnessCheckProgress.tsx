// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import './harness-check-progress.css'

export function HarnessCheckProgress({ label }: { label: string }) {
  const [started] = useState(() => Date.now())
  const [elapsed, setElapsed] = useState(0)
  useEffect(() => {
    const timer = window.setInterval(
      () => setElapsed(Date.now() - started),
      100
    )
    return () => window.clearInterval(timer)
  }, [started])
  return (
    <div
      role="status"
      className="flex items-center gap-2.5 text-muted-foreground"
    >
      <span
        aria-hidden="true"
        className="grid shrink-0 grid-cols-3 gap-[1.5px]"
      >
        {Array.from({ length: 9 }, (_, index) => (
          <span
            key={index}
            className="harness-check-pixel size-1 rounded-[1px] bg-current"
            style={{
              animationDelay: `${((index % 3) + Math.abs(Math.floor(index / 3) - 1)) * 90}ms`,
            }}
          />
        ))}
      </span>
      <span className="text-sm">{label}</span>
      <span aria-hidden="true" className="font-mono text-xs tabular-nums">
        {(elapsed / 1000).toFixed(1)}s
      </span>
    </div>
  )
}
