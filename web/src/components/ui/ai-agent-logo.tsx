import { Bot } from 'lucide-react'

/**
 * Maps either a canonical AI provider name (e.g. "OpenAI") or a specific agent
 * name (e.g. "GPTBot", "Claude-User") to its logo file in `public/ai-agents/`.
 * Provider-level matches take precedence — every agent from a provider should
 * render with the same logo unless we have one specific to the agent.
 */
const PROVIDER_TO_LOGO: Record<string, string> = {
  OpenAI: 'openai.svg',
  Anthropic: 'anthropic.svg',
  Perplexity: 'perplexity.svg',
  Google: 'google.svg',
  Apple: 'apple.svg',
  Meta: 'meta.svg',
  Amazon: 'amazon.svg',
  ByteDance: 'bytedance.svg',
  'Common Crawl': 'common-crawl.svg',
  Cohere: 'cohere.svg',
  Diffbot: 'diffbot.svg',
  'You.com': 'you.svg',
  DuckDuckGo: 'duckduckgo.svg',
  Brave: 'brave.svg',
  Andi: 'andi.svg',
  Omgili: 'omgili.svg',
  ImageSift: 'imagesift.svg',
  Timpi: 'timpi.svg',
  Kangaroo: 'kangaroo.svg',
  Mistral: 'mistral.svg',
  xAI: 'xai.svg',
}

const AGENT_TO_LOGO: Record<string, string> = {
  GPTBot: 'openai.svg',
  'OAI-SearchBot': 'openai.svg',
  'ChatGPT-User': 'openai.svg',
  OpenAI: 'openai.svg',
  ClaudeBot: 'anthropic.svg',
  'Claude-SearchBot': 'anthropic.svg',
  'Claude-User': 'anthropic.svg',
  'anthropic-ai': 'anthropic.svg',
  PerplexityBot: 'perplexity.svg',
  'Perplexity-User': 'perplexity.svg',
  GoogleOther: 'google.svg',
  Googlebot: 'google.svg',
  'Gemini-Deep-Research': 'google.svg',
  Applebot: 'apple.svg',
  'Applebot-Extended': 'apple.svg',
  'Meta-ExternalAgent': 'meta.svg',
  'Meta-ExternalFetcher': 'meta.svg',
  Amazonbot: 'amazon.svg',
  Bytespider: 'bytedance.svg',
  CCBot: 'common-crawl.svg',
  'cohere-ai': 'cohere.svg',
  'cohere-training-data-crawler': 'cohere.svg',
  Diffbot: 'diffbot.svg',
  YouBot: 'you.svg',
  DuckAssistBot: 'duckduckgo.svg',
  BraveBot: 'brave.svg',
  Andibot: 'andi.svg',
  Omgilibot: 'omgili.svg',
  Omgili: 'omgili.svg',
  ImagesiftBot: 'imagesift.svg',
  Timpibot: 'timpi.svg',
  'Kangaroo Bot': 'kangaroo.svg',
  'MistralAI-User': 'mistral.svg',
  GrokBot: 'xai.svg',
  'xAI-Grok': 'xai.svg',
  // Bingbot (Microsoft) and DeepSeekBot (DeepSeek) have no brand SVG in
  // public/ai-agents/ yet, so they fall through to the generic <Bot> icon
  // in AiAgentLogo rather than reference a file that doesn't exist.
}

interface AiAgentLogoProps {
  /** Provider name (e.g. "OpenAI"). Preferred when available. */
  provider?: string | null
  /** Agent name (e.g. "GPTBot"). Used as a fallback when provider is unknown. */
  agent?: string | null
  size?: number
  className?: string
}

export function AiAgentLogo({
  provider,
  agent,
  size = 16,
  className,
}: AiAgentLogoProps) {
  const file =
    (provider && PROVIDER_TO_LOGO[provider]) ||
    (agent && AGENT_TO_LOGO[agent]) ||
    null

  if (file) {
    // The brand SVGs are monochrome black `<path>`s (and our placeholders use
    // `currentColor`). Loaded via `<img>`, neither can inherit the page text
    // colour, so on a dark background they'd render black-on-black. Painting
    // them on a fixed white chip keeps every mark legible in both themes —
    // brand logos are designed to sit on white anyway. The chip is padded so
    // the glyph never touches the edge.
    const pad = Math.max(2, Math.round(size * 0.18))
    return (
      <span
        className={`inline-flex shrink-0 items-center justify-center rounded-[4px] bg-white ring-1 ring-black/5 ${className ?? ''}`}
        style={{ width: size, height: size, padding: pad }}
      >
        <img
          src={`/ai-agents/${file}`}
          alt={provider || agent || 'AI Agent'}
          width={size - pad * 2}
          height={size - pad * 2}
          style={{
            width: size - pad * 2,
            height: size - pad * 2,
            display: 'block',
            color: '#111',
          }}
        />
      </span>
    )
  }

  return <Bot width={size} height={size} className={className} />
}
