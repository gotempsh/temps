// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { AiHarnessLogo } from '@/components/ui/ai-harness-logo'

/** A row of small app logos, for a choice that covers several apps. */
export function LogoStack({
  ids,
  size = 18,
}: {
  ids: string[]
  size?: number
}) {
  return (
    <span className="flex flex-wrap items-center gap-1.5">
      {ids.map((id) => (
        <AiHarnessLogo key={id} providerId={id} size={size} />
      ))}
    </span>
  )
}
