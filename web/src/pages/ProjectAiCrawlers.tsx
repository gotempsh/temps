// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageHeader } from '@/components/layout/PageContainer'
import { ProjectResponse } from '@/api/client'
import { AiCrawlerActivityFeed } from '@/components/analytics/AiCrawlerActivityFeed'

interface ProjectAiCrawlersProps {
  project: ProjectResponse
}

/**
 * Project-scoped AI crawler activity — a chronological feed of AI-agent
 * requests (ClaudeBot, GPTBot, PerplexityBot, …) hitting this project's sites,
 * newest first. Lives under the project Observe section, next to Request Logs.
 */
export default function ProjectAiCrawlers({ project }: ProjectAiCrawlersProps) {
  return (
    <div className="w-full space-y-6">
      <PageHeader
        title="AI crawlers"
        description="Requests from AI crawlers (ChatGPT, Claude, Perplexity, and more) fetching this service, newest first."
      />
      <AiCrawlerActivityFeed projectId={project.id} />
    </div>
  )
}
