// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { Sparkles } from 'lucide-react'
import { useLocation, useNavigate } from 'react-router'
import { ASSISTANT_URL, assistantHref } from '@/lib/assistant-entry'

/**
 * The top bar's one button: "Ask", the same entry the sidebar has, for when
 * the sidebar is folded or off screen. It opens the assistant (`/chat`):
 * onboarding when no AI provider is set up, then the last chat or a new one.
 * Pressing it again returns to where the user was.
 *
 * Always shown: gateway keys are only one possible runtime, and hiding this
 * button also hid managed Cloud and host-authenticated Claude/Codex/OpenCode.
 * When no provider is ready, the page itself renders the setup state and direct
 * settings link instead of making the feature undiscoverable.
 *
 * Contextual "Ask AI about this" buttons elsewhere still open the side dock
 * (ADR-023) so the page being discussed stays visible.
 */
export function AiAssistantButton() {
  const navigate = useNavigate()
  const { pathname } = useLocation()
  const active = pathname === ASSISTANT_URL

  const toggle = () => {
    if (!active) {
      navigate(assistantHref())
      return
    }
    // `idx` is react-router's position in this tab's history; 0 means there is
    // nothing to go back to (a direct visit or reload), so land on a real page.
    const idx = (window.history.state as { idx?: number } | null)?.idx ?? 0
    if (idx > 0) navigate(-1)
    else navigate('/projects')
  }

  return (
    <Button
      variant={active ? 'secondary' : 'outline'}
      onClick={toggle}
      aria-pressed={active}
      className="h-9 gap-1.5 px-2.5 sm:px-3"
    >
      <Sparkles className="size-4" />
      Ask
    </Button>
  )
}
