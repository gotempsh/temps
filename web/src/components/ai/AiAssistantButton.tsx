// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useAiAssistant } from '@/components/ai/AiAssistantContext'
import { Button } from '@/components/ui/button'
import { Sparkles } from 'lucide-react'

/**
 * Global top-bar entry point to the persistent AI assistant dock (ADR-023).
 * Always shown: gateway keys are only one possible runtime, and hiding this
 * button also hid valid host-authenticated Claude/Codex/OpenCode providers.
 * When no provider is ready, the dock itself renders the setup state and direct
 * settings link instead of making the feature undiscoverable.
 */
export function AiAssistantButton() {
  const { open, close, isOpen } = useAiAssistant()

  return (
    <Button
      variant={isOpen ? 'secondary' : 'outline'}
      size="icon"
      onClick={() => (isOpen ? close() : open())}
      title="AI assistant"
      aria-pressed={isOpen}
    >
      <Sparkles className="h-4 w-4" />
      <span className="sr-only">AI assistant</span>
    </Button>
  )
}
