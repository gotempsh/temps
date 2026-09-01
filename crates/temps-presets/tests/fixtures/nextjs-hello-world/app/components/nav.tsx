// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import Link from 'next/link'
import { usePathname } from 'next/navigation'

const links = [
  { href: '/', label: 'Home' },
  { href: '/counter', label: 'Counter' },
  { href: '/about', label: 'About' },
  { href: '/todos', label: 'Todos' },
]

export default function Nav() {
  const pathname = usePathname()

  return (
    <nav style={{ display: 'flex', gap: '1rem', padding: '1rem', borderBottom: '1px solid #ccc' }}>
      {links.map(({ href, label }) => (
        <Link
          key={href}
          href={href}
          style={{ fontWeight: pathname === href ? 'bold' : 'normal' }}
        >
          {label}
        </Link>
      ))}
    </nav>
  )
}
