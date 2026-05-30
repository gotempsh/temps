import { useEffect } from 'react'
import { AiCrawlerActivityFeed } from '@/components/analytics/AiCrawlerActivityFeed'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

export default function AiCrawlerActivity() {
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([{ label: 'AI Crawlers' }])
  }, [setBreadcrumbs])

  usePageTitle('AI Crawler Activity')

  return (
    <div className="container max-w-7xl mx-auto py-8">
      <div className="space-y-6">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">
            AI Crawler Activity
          </h2>
          <p className="text-muted-foreground">
            Requests from AI crawlers (ChatGPT, Claude, Perplexity, and more)
            fetching your sites, newest first.
          </p>
        </div>
        <AiCrawlerActivityFeed />
      </div>
    </div>
  )
}
