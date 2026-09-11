import * as React from 'react'
import { Globe, Link } from 'lucide-react'

interface ReferrerIconProps {
  domain: string
  className?: string
}

export function ReferrerIcon({
  domain,
  className = 'h-5 w-5',
}: ReferrerIconProps) {
  const [hasError, setHasError] = React.useState(false)

  if (!domain || domain === 'Direct') {
    return <Link className={`${className} text-muted-foreground`} />
  }

  if (hasError) {
    return <Globe className={`${className} text-muted-foreground`} />
  }

  const faviconDomain = ['twitter.com', 't.co'].includes(domain)
    ? 'x.com'
    : domain
  const faviconUrl = `https://www.google.com/s2/favicons?domain=${encodeURIComponent(faviconDomain)}&sz=32`

  return (
    <img
      src={faviconUrl}
      alt={`${domain} favicon`}
      className={`${className} rounded-sm bg-white object-contain`}
      onError={() => setHasError(true)}
    />
  )
}

export function getReferrerDisplayName(hostname: string): string {
  if (!hostname || hostname === 'Direct') return 'Direct'

  if (hostname.startsWith('google.') || hostname.startsWith('www.google.')) {
    return 'Google'
  }
  if (hostname === 'accounts.google.com') return 'Google'
  if (hostname === 'mail.google.com') return 'Gmail'

  const commonSites: Record<string, string> = {
    'bing.com': 'Bing',
    'cn.bing.com': 'Bing',
    'www.bing.com': 'Bing',
    'baidu.com': 'Baidu',
    'www.baidu.com': 'Baidu',
    'naver.com': 'Naver',
    'm.search.naver.com': 'Naver',
    'search.naver.com': 'Naver',
    'www.naver.com': 'Naver',
    'facebook.com': 'Facebook',
    'www.facebook.com': 'Facebook',
    'm.facebook.com': 'Facebook',
    'l.facebook.com': 'Facebook',
    'lm.facebook.com': 'Facebook',
    'instagram.com': 'Instagram',
    'www.instagram.com': 'Instagram',
    'l.instagram.com': 'Instagram',
    'youtube.com': 'YouTube',
    'www.youtube.com': 'YouTube',
    'reddit.com': 'Reddit',
    'www.reddit.com': 'Reddit',
    'out.reddit.com': 'Reddit',
    'twitter.com': 'X',
    'x.com': 'X',
    't.co': 'X',
    'linkedin.com': 'LinkedIn',
    'www.linkedin.com': 'LinkedIn',
    'github.com': 'GitHub',
    'www.github.com': 'GitHub',
    'duckduckgo.com': 'DuckDuckGo',
    'www.duckduckgo.com': 'DuckDuckGo',
    'yandex.ru': 'Yandex',
    'ya.ru': 'Yandex',
    'yahoo.com': 'Yahoo',
    'search.yahoo.com': 'Yahoo',
    'www.yahoo.com': 'Yahoo',
    'tiktok.com': 'TikTok',
    'www.tiktok.com': 'TikTok',
    'pinterest.com': 'Pinterest',
    'www.pinterest.com': 'Pinterest',
    'chatgpt.com': 'ChatGPT',
    'www.chatgpt.com': 'ChatGPT',
    'perplexity.ai': 'Perplexity',
    'www.perplexity.ai': 'Perplexity',
    'news.ycombinator.com': 'Hacker News',
    'stripe.com': 'Stripe',
    'checkout.stripe.com': 'Stripe',
    'substack.com': 'Substack',
    'discord.com': 'Discord',
    'www.discord.com': 'Discord',
    'wikipedia.org': 'Wikipedia',
    'en.wikipedia.org': 'Wikipedia',
    'www.wikipedia.org': 'Wikipedia',
    'slack.com': 'Slack',
    'app.slack.com': 'Slack',
    'notion.so': 'Notion',
    'www.notion.so': 'Notion',
  }

  return commonSites[hostname] || hostname
}
