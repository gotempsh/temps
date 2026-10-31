// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'
import { ipDetailPath } from '@/lib/ip-detail'
import { cn } from '@/lib/utils'

/**
 * A client address that opens its IP page. Clicks stop here, so a link inside
 * a clickable row opens the address rather than the row.
 */
export function IpLink({ ip, className }: { ip: string; className?: string }) {
  return (
    <Link
      to={ipDetailPath(ip)}
      onClick={(event) => event.stopPropagation()}
      className={cn('underline-offset-4 hover:underline', className)}
      title={`Where ${ip} is, and what else it requested`}
    >
      {ip}
    </Link>
  )
}
