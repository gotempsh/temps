// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useNavigate } from 'react-router'
import { DockBody } from '@/components/ai/AiAssistantDock'
import { useAiAssistant } from '@/components/ai/AiAssistantContext'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

/**
 * Full-screen AI assistant.
 *
 * Same component as the dock (`DockBody`) with the whole content area instead
 * of a 28rem column — for long diagnostic conversations where the dock is too
 * narrow to read comfortably. The dock stays the default for asking something
 * while you keep working on a page; this is the "sit and read it" surface.
 */
export function AiChat() {
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const { close, initialContext } = useAiAssistant()

  usePageTitle('AI Chat')

  useEffect(() => {
    setBreadcrumbs([{ label: 'AI Chat' }])
  }, [setBreadcrumbs])

  // The dock and this page render the same assistant; showing both at once
  // would duplicate the conversation on screen.
  useEffect(() => {
    close()
  }, [close])

  // Fill the content area exactly. The shell already bounds this page (a
  // `dvh`-tall column whose content slot is whatever is left under the header
  // and any banners), so `h-full` tracks that remaining height. Guessing it with
  // viewport math instead overflowed by the banner and container-padding heights
  // and pushed the composer below the fold.
  return (
    <div className="h-full min-h-0">
      {/* `initialContext` is set when the user expands a dock conversation to
          full screen, so this lands on that same thread; it is undefined on a
          direct visit, which opens on the chat list. */}
      <DockBody
        layout="page"
        initialContext={initialContext}
        onClose={() => navigate('/projects')}
      />
    </div>
  )
}
