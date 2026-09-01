// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { UploadCloud } from 'lucide-react'
import { Link, useLocation } from 'react-router'

/**
 * Global top-bar entry point to the Drop tab in New Project — deploy by dropping a folder or ZIP,
 * no Git remote and no CLI.
 *
 * Drop is reachable from the Projects header, the empty-state onboarding, the
 * New Project shell and the command palette, but all four of those live inside
 * the project-creation flow. This button is the one surface that follows you
 * across the whole console, so "I have a folder on my desk, ship it" is always
 * one click away rather than something you have to navigate back to Projects
 * to find.
 *
 * `UploadCloud` is deliberate, not decorative: every other Drop surface already
 * uses it, so the icon alone identifies the feature once you've seen it once.
 * A different glyph here would fragment that association.
 *
 * Unconditional by design — Drop needs no operator configuration, so there is
 * no "not set up" state to gate on.
 */
export function DropButton() {
  const { pathname } = useLocation()
  const isActive = pathname === '/projects/new'

  return (
    <Button
      asChild
      variant={isActive ? 'secondary' : 'outline'}
      size="icon"
      title="Drop files to deploy"
      aria-current={isActive ? 'page' : undefined}
    >
      <Link to="/projects/new?source=drop">
        <UploadCloud className="size-4" />
        <span className="sr-only">Drop files to deploy</span>
      </Link>
    </Button>
  )
}
