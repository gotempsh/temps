import { useAiAssistant } from '@/components/ai/AiAssistantContext'
import { Button } from '@/components/ui/button'
import { Sparkles } from 'lucide-react'

interface DebugChatProps {
  projectId: number
  /** The interaction this chat is attached to, e.g. 'deployment' | 'alert'. */
  contextType: string
  contextId: string | number
  title?: string
  description?: string
  /** Auto-asked when a new chat is started, so it opens already working. */
  startPrompt?: string
  triggerLabel?: string
  /** For the dock's "view source" link. */
  projectSlug?: string
  projectName?: string
}

/**
 * A standalone "Debug with AI" trigger that opens the persistent assistant dock
 * (ADR-023) straight into this entity's conversation. Render only when the
 * project's `ai_debug_chat_enabled` toggle is on.
 */
export function DebugChat({
  projectId,
  contextType,
  contextId,
  title = 'Debug with AI',
  description,
  startPrompt = 'Diagnose this and suggest concrete next steps.',
  triggerLabel = 'Debug with AI',
  projectSlug,
  projectName,
}: DebugChatProps) {
  const { open } = useAiAssistant()
  return (
    <Button
      variant="outline"
      className="gap-2"
      onClick={() =>
        open({
          projectId,
          context: {
            contextType,
            contextId,
            title,
            description,
            startPrompt,
            projectSlug,
            projectName,
          },
        })
      }
    >
      <Sparkles className="h-4 w-4 text-primary" />
      {triggerLabel}
    </Button>
  )
}
