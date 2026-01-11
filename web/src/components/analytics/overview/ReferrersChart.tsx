import { getPropertyBreakdownOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import { Globe, Link } from 'lucide-react'
import * as React from 'react'

interface ReferrerIconProps {
  domain: string
  className?: string
}

function ReferrerIcon({ domain, className = 'h-5 w-5' }: ReferrerIconProps) {
  const [hasError, setHasError] = React.useState(false)

  // For direct traffic or empty domains, show Link icon
  if (!domain || domain === 'Direct / None') {
    return <Link className={`${className} text-muted-foreground`} />
  }

  // If favicon failed to load, show Globe icon
  if (hasError) {
    return <Globe className={`${className} text-muted-foreground`} />
  }

  // Use Google's favicon service
  const faviconUrl = `https://www.google.com/s2/favicons?domain=${encodeURIComponent(domain)}&sz=32`

  return (
    <img
      src={faviconUrl}
      alt={`${domain} favicon`}
      className={className}
      onError={() => setHasError(true)}
    />
  )
}

function getDisplayName(hostname: string): string {
  if (!hostname || hostname === 'Direct / None') return 'Direct / None'

  // Handle Google domains
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
    'twitter.com': 'Twitter',
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

interface ReferrersChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function ReferrersChart({
  project,
  startDate,
  endDate,
  environment,
}: ReferrersChartProps) {
  const { data, isLoading, error } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: 'referrer_hostname',
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 10,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const sortedReferrers = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
      .sort((a, b) => b.count - a.count)
      .map((referrer) => {
        const hostname = referrer.value || 'Direct / None'
        return {
          hostname,
          displayName: getDisplayName(hostname),
          count: referrer.count,
          percentage: ((referrer.count / total) * 100).toFixed(1),
        }
      })
  }, [data])

  return (
    <Card>
      <CardHeader>
        <CardTitle>Referrers</CardTitle>
        <CardDescription>
          {startDate && endDate
            ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
            : 'Select a date range'}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              {[...Array(5)].map((_, i) => (
                <div key={i} className="flex items-center justify-between">
                  <div className="h-4 w-[150px] bg-muted animate-pulse rounded" />
                  <div className="h-4 w-[100px] bg-muted animate-pulse rounded" />
                </div>
              ))}
            </div>
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load referrer analytics
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !sortedReferrers.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No data available for the selected period
            </p>
          </div>
        ) : (
          <div className="space-y-3" style={{ minHeight: '400px' }}>
            {sortedReferrers.map((referrer) => (
              <div key={referrer.hostname} className="space-y-2">
                <div className="flex items-center justify-between gap-4">
                  <div className="flex items-center gap-3 min-w-0 flex-1">
                    <ReferrerIcon domain={referrer.hostname} className="h-5 w-5 shrink-0" />
                    <span className="text-sm font-medium truncate">
                      {referrer.displayName}
                    </span>
                  </div>
                  <div className="flex items-center gap-2 shrink-0">
                    <span className="text-sm text-muted-foreground">
                      {referrer.percentage}%
                    </span>
                    <span className="text-sm font-mono text-muted-foreground">
                      {referrer.count.toLocaleString()}
                    </span>
                  </div>
                </div>
                <div className="relative h-2 bg-muted rounded-full overflow-hidden">
                  <div
                    className="absolute inset-y-0 left-0 bg-primary rounded-full transition-all duration-500"
                    style={{ width: `${referrer.percentage}%` }}
                  />
                </div>
              </div>
            ))}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && sortedReferrers.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {sortedReferrers.length} referrers by unique visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}
