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
    <div className="w-full py-4 sm:py-6">
      <div className="mb-4 space-y-1">
        <h2 className="text-2xl font-bold tracking-tight">AI Crawlers</h2>
        <p className="text-sm text-muted-foreground">
          Requests from AI crawlers (ChatGPT, Claude, Perplexity, and more)
          fetching this project, newest first.
        </p>
      </div>
      <AiCrawlerActivityFeed projectId={project.id} />
    </div>
  )
}
