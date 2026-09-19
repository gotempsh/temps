// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useId, type ReactNode } from 'react'

/** Open settings group: section identity left, controls right; stacked on mobile. */
export function SettingsGroup({
  title,
  children,
}: {
  title: string
  children: ReactNode
}) {
  const id = useId()
  return (
    <section
      aria-labelledby={id}
      className="grid min-w-0 gap-5 md:grid-cols-[minmax(0,1fr)_minmax(0,2fr)] md:gap-10"
    >
      <h2 id={id} className="text-base font-semibold">
        {title}
      </h2>
      <div className="min-w-0 space-y-5">{children}</div>
    </section>
  )
}
