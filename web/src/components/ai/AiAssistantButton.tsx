import { listProviderKeys } from '@/api/client'
import { useAiAssistant } from '@/components/ai/AiAssistantContext'
import { Button } from '@/components/ui/button'
import { useQuery } from '@tanstack/react-query'
import { Sparkles } from 'lucide-react'

/**
 * Global top-bar entry point to the persistent AI assistant dock (ADR-023).
 * Shown on every page whenever an AI provider is configured — the dock opens on
 * the cross-project conversation list, so any chat can be resumed from anywhere
 * (it lives in the app shell and stays open while you navigate). Starting new
 * chats still happens from a failed deployment stage or a firing alert.
 */
export function AiAssistantButton() {
  const { open, close, isOpen } = useAiAssistant()
  // Shared cache key with AiGateway / AiProvidersPage — no extra fetch.
  const { data: keys } = useQuery({
    queryKey: ['providerKeys'],
    queryFn: async () => (await listProviderKeys()).data ?? [],
    staleTime: 60_000,
    retry: false,
  })

  const aiConfigured = (keys ?? []).some((k) => k.is_active)
  if (!aiConfigured) return null

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
