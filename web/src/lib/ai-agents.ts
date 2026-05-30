/**
 * Mirror of the backend AI-agent taxonomy (`temps-proxy::ai_agent_detector`).
 * Keep in sync when adding providers there. We hardcode here so dropdowns and
 * provider lookups don't need an extra round-trip on first paint; the backend
 * remains the source of truth for matching at ingest time.
 *
 * Shared by the request-log filters (`ProxyLogsDataTable`) and the AI Crawler
 * Activity feed so the list isn't duplicated in two places.
 */
export const AI_PROVIDERS: { provider: string; agents: string[] }[] = [
  {
    provider: 'OpenAI',
    agents: ['GPTBot', 'OAI-SearchBot', 'ChatGPT-User', 'OpenAI'],
  },
  {
    provider: 'Anthropic',
    agents: ['ClaudeBot', 'Claude-SearchBot', 'Claude-User', 'anthropic-ai'],
  },
  { provider: 'Perplexity', agents: ['PerplexityBot', 'Perplexity-User'] },
  { provider: 'Google', agents: ['GoogleOther'] },
  { provider: 'Apple', agents: ['Applebot', 'Applebot-Extended'] },
  { provider: 'Meta', agents: ['Meta-ExternalAgent', 'Meta-ExternalFetcher'] },
  { provider: 'Amazon', agents: ['Amazonbot'] },
  { provider: 'ByteDance', agents: ['Bytespider'] },
  { provider: 'Common Crawl', agents: ['CCBot'] },
  { provider: 'Cohere', agents: ['cohere-ai', 'cohere-training-data-crawler'] },
  { provider: 'Diffbot', agents: ['Diffbot'] },
  { provider: 'You.com', agents: ['YouBot'] },
  { provider: 'DuckDuckGo', agents: ['DuckAssistBot'] },
  { provider: 'Brave', agents: ['Bravebot'] },
  { provider: 'Andi', agents: ['Andibot'] },
  { provider: 'Omgili', agents: ['Omgilibot', 'Omgili'] },
  { provider: 'ImageSift', agents: ['ImagesiftBot'] },
  { provider: 'Timpi', agents: ['Timpibot'] },
  { provider: 'Kangaroo', agents: ['Kangaroo Bot'] },
  { provider: 'Mistral', agents: ['MistralAI-User'] },
  { provider: 'xAI', agents: ['GrokBot'] },
]

/** Canonical agent name -> provider name (e.g. `"ClaudeBot"` -> `"Anthropic"`). */
export const AGENT_TO_PROVIDER: Record<string, string> = AI_PROVIDERS.reduce(
  (acc, { provider, agents }) => {
    for (const agent of agents) acc[agent] = provider
    return acc
  },
  {} as Record<string, string>
)
