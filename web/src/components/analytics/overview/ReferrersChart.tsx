import { getPropertyBreakdownOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
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
import { ChevronLeft, Globe, Link } from 'lucide-react'
import * as React from 'react'

interface ReferrerIconProps {
  domain: string
  className?: string
}

function ReferrerIcon({ domain, className = 'h-5 w-5' }: ReferrerIconProps) {
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
      className={className}
      onError={() => setHasError(true)}
    />
  )
}

function getDisplayName(hostname: string): string {
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
  const [selectedReferrer, setSelectedReferrer] = React.useState<string | null>(
    null,
  )

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

  // When a referrer is selected, show channel breakdown for context
  const { data: channelData } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: 'pathname',
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 5,
      },
    }),
    enabled: !!selectedReferrer && !!startDate && !!endDate,
  })

  const sortedReferrers = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
      .sort((a, b) => b.count - a.count)
      .map((referrer) => {
        const hostname = referrer.value || 'Direct'
        return {
          hostname,
          displayName: getDisplayName(hostname),
          count: referrer.count,
          percentage: ((referrer.count / total) * 100).toFixed(1),
        }
      })
  }, [data])

  // Detail view for a selected referrer
  if (selectedReferrer) {
    const referrer = sortedReferrers.find(
      (r) => r.hostname === selectedReferrer,
    )
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="icon"
              className="h-6 w-6"
              onClick={() => setSelectedReferrer(null)}
            >
              <ChevronLeft className="h-4 w-4" />
            </Button>
            <ReferrerIcon domain={selectedReferrer} className="h-5 w-5" />
            {getDisplayName(selectedReferrer)}
          </CardTitle>
          <CardDescription>
            {referrer
              ? `${referrer.count.toLocaleString()} visitors (${referrer.percentage}%)`
              : ''}
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="space-y-4">
            <div>
              <p className="text-sm font-medium mb-1 text-muted-foreground">
                Hostname
              </p>
              <p className="text-sm font-mono">{selectedReferrer}</p>
            </div>
            {selectedReferrer !== 'Direct' && (
              <div>
                <Badge variant="outline" className="text-xs">
                  <a
                    href={`https://${selectedReferrer}`}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="flex items-center gap-1"
                  >
                    Visit site
                    <Globe className="h-3 w-3" />
                  </a>
                </Badge>
              </div>
            )}
            {channelData && channelData.items.length > 0 && (
              <div>
                <p className="text-sm font-medium mb-2 text-muted-foreground">
                  Top Pages (all traffic)
                </p>
                <div className="space-y-2">
                  {channelData.items.slice(0, 5).map((page) => (
                    <div
                      key={page.value}
                      className="flex items-center justify-between text-sm"
                    >
                      <span className="font-mono truncate max-w-[200px]">
                        {page.value || '/'}
                      </span>
                      <span className="text-muted-foreground">
                        {page.count.toLocaleString()}
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>
        </CardContent>
      </Card>
    )
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle>Referrers</CardTitle>
            <CardDescription>
              {startDate && endDate
                ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                : 'Select a date range'}
            </CardDescription>
          </div>
          <Badge variant="outline" className="text-xs">
            Click for details
          </Badge>
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              {[...Array(5)].map((_, i) => (
                <div
                  key={`skeleton-ref-${i}`}
                  className="flex items-center justify-between"
                >
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
              <button
                type="button"
                key={referrer.hostname}
                className="space-y-2 w-full text-left cursor-pointer hover:bg-muted/50 rounded-lg p-1 -mx-1"
                onClick={() => setSelectedReferrer(referrer.hostname)}
              >
                <div className="flex items-center justify-between gap-4">
                  <div className="flex items-center gap-3 min-w-0 flex-1">
                    <ReferrerIcon
                      domain={referrer.hostname}
                      className="h-5 w-5 shrink-0"
                    />
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
              </button>
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
