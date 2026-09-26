// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Sparkles } from 'lucide-react'
import { PageContainer, PageHeader, PageState } from '@temps-sdk/ds'

/**
 * Reference screen for `PageState`'s `not-set-up` variant — the CLAUDE.md
 * rule made concrete: a feature that depends on optional operator config
 * must always show the surface, say what's missing, give a concrete example,
 * and link straight to the setting. Never render nothing.
 */
export default function AiSummaries() {
  return (
    <PageContainer>
      <PageHeader
        title="AI deploy summaries"
        description="One-line, plain-English summaries of what each deploy actually changed."
      />
      <PageState
        variant="not-set-up"
        icon={Sparkles}
        title="No AI provider configured"
        requirement="Deploy summaries need an AI provider (Anthropic, OpenAI, or a self-hosted model) configured for this project."
        example={
          <>
            "Bumped the checkout retry timeout from 3s to 8s and added a
            fallback provider for card declines."
          </>
        }
        settingsHref="/settings/ai"
        settingsLabel="Configure AI provider"
      />
    </PageContainer>
  )
}
