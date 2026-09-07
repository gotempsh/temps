// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import type { TemplateResponse } from '@/api/client/types.gen'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Star, Database, Server } from 'lucide-react'
import { cn } from '@/lib/utils'
import { TemplateImage } from './TemplateImage'

interface TemplateCardProps {
  template: TemplateResponse
  onClick?: (template: TemplateResponse) => void
  selected?: boolean
  /**
   * Drop the screenshot banner. Set for the full-width list view, where a
   * 16:9 banner would be several hundred pixels tall per row.
   */
  compact?: boolean
}

interface TemplateScreenshotProps {
  src: string
  alt: string
}

/**
 * Wide screenshot banner shown above the card header. Screenshots are captured
 * from real running instances (tall, full-page), so the banner crops to the top
 * of the image to keep the hero/first section visible.
 *
 * Renders nothing at all if the image fails to load — most templates have no
 * screenshot yet, and a broken-image icon would look worse than no banner.
 */
function TemplateScreenshot({ src, alt }: TemplateScreenshotProps) {
  const [failed, setFailed] = useState(false)

  if (failed) return null

  return (
    <div className="aspect-video w-full overflow-hidden border-b bg-muted">
      <img
        src={src}
        alt={`${alt} preview`}
        loading="lazy"
        className="h-full w-full object-cover object-top"
        onError={() => setFailed(true)}
      />
    </div>
  )
}

/** Map service names to display-friendly labels */
function getServiceLabel(service: string): string {
  const labels: Record<string, string> = {
    postgres: 'PostgreSQL',
    mysql: 'MySQL',
    mariadb: 'MariaDB',
    redis: 'Redis',
    mongodb: 'MongoDB',
    minio: 'MinIO (Deprecated)',
    rustfs: 'RustFS',
    rabbitmq: 'RabbitMQ',
    memcached: 'Memcached',
    clickhouse: 'ClickHouse',
    influxdb: 'InfluxDB',
    cassandra: 'Cassandra',
    neo4j: 'Neo4j',
    opensearch: 'OpenSearch',
    valkey: 'Valkey',
  }
  return labels[service.toLowerCase()] || service
}

export function TemplateCard({
  template,
  onClick,
  selected,
  compact,
}: TemplateCardProps) {
  return (
    <Card
      className={cn(
        onClick &&
          'cursor-pointer overflow-hidden transition-all hover:border-primary/50 hover:shadow-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2',
        selected && 'border-primary ring-2 ring-primary/20'
      )}
      onClick={() => onClick?.(template)}
      role={onClick ? 'button' : undefined}
      tabIndex={onClick ? 0 : undefined}
      aria-pressed={onClick ? Boolean(selected) : undefined}
      onKeyDown={(event) => {
        if (!onClick || (event.key !== 'Enter' && event.key !== ' ')) return
        event.preventDefault()
        onClick(template)
      }}
    >
      {!compact && template.screenshot_url && (
        <TemplateScreenshot src={template.screenshot_url} alt={template.name} />
      )}
      <CardHeader className="pb-2">
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-3">
            <TemplateImage
              imageUrl={template.image_url}
              preset={template.preset}
              alt={template.name}
              className="h-10 w-10"
              imgClassName="h-8 w-8"
            />
            <div>
              <CardTitle className="text-base font-semibold flex items-center gap-2">
                {template.name}
                {template.is_featured && (
                  <Star className="h-4 w-4 text-yellow-500 fill-yellow-500" />
                )}
              </CardTitle>
              <p className="text-xs text-muted-foreground">{template.preset}</p>
            </div>
          </div>
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        {template.description && (
          <p className="text-sm text-muted-foreground line-clamp-2">
            {template.description}
          </p>
        )}

        {/* Tags */}
        {template.tags.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {template.tags.slice(0, 4).map((tag) => (
              <Badge key={tag} variant="secondary" className="text-xs">
                {tag}
              </Badge>
            ))}
            {template.tags.length > 4 && (
              <Badge variant="outline" className="text-xs">
                +{template.tags.length - 4}
              </Badge>
            )}
          </div>
        )}

        {/* Services */}
        {template.services.length > 0 && (
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <Database className="h-3 w-3" />
            <span>{template.services.map(getServiceLabel).join(', ')}</span>
          </div>
        )}

        {/* Features preview */}
        {template.features.length > 0 && (
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <Server className="h-3 w-3" />
            <span className="line-clamp-1">
              {template.features.slice(0, 2).join(' · ')}
              {template.features.length > 2 &&
                ` +${template.features.length - 2}`}
            </span>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
