// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `CopyButton` (web/src/components/ui/copy-button.tsx) already answers
// "copied" on itself via its own tooltip state — never a toast — so its
// behavior is exactly `CopyAction`; reused, not duplicated. It ships with no
// default padding or radius (every existing call site supplies its own), so
// this thin wrapper gives `CopyAction` a sane icon-button default: its own
// small hover target with padding, never the bare `size-4` icon alone. Pass
// `children` only for a labeled button — an icon-only copy target must never
// share its clickable/hover region with the value it copies; put the value
// in its own element beside `CopyAction`, not inside it.
import type { ComponentProps } from 'react'
import { CopyButton } from '../../../src/components/ui/copy-button'
import { cn } from './lib/cn'

export function CopyAction({
  className,
  ...props
}: ComponentProps<typeof CopyButton>) {
  return <CopyButton className={cn('rounded-md p-1.5', className)} {...props} />
}
