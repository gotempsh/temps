// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { Link as RouterLink } from 'react-router'
import { isInternalConsolePath } from '@/lib/cloud-telemetry-activation'

/**
 * Text that links into the console when its `href` is a console path. A
 * model wrote the address, so anything that would leave the console (a
 * scheme, `//host`) is shown as the plain text it came with.
 */
export function ConsoleLink({
  href,
  children,
}: {
  href: string | null | undefined
  children: ReactNode
}) {
  if (!isInternalConsolePath(href)) return <>{children}</>
  return (
    <RouterLink
      to={href}
      className="font-medium text-foreground underline decoration-border underline-offset-4 hover:decoration-foreground"
    >
      {children}
    </RouterLink>
  )
}
