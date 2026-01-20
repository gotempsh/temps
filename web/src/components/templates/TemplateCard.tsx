import type { TemplateResponse } from '@/api/client/types.gen'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Star, Database, Server } from 'lucide-react'
import { cn } from '@/lib/utils'

interface TemplateCardProps {
  template: TemplateResponse
  onClick?: (template: TemplateResponse) => void
  selected?: boolean
}

/** Map service names to display-friendly labels */
function getServiceLabel(service: string): string {
  const labels: Record<string, string> = {
    postgres: 'PostgreSQL',
    mysql: 'MySQL',
    mariadb: 'MariaDB',
    redis: 'Redis',
    mongodb: 'MongoDB',
    minio: 'MinIO',
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

/** Map preset names to icon URLs */
function getPresetIcon(preset: string): string {
  const icons: Record<string, string> = {
    nextjs: '/presets/nextjs.svg',
    fastapi: '/presets/fastapi.svg',
    django: '/presets/django.svg',
    remix: '/presets/remix.svg',
    nuxt: '/presets/nuxt.svg',
    astro: '/presets/astro.svg',
    rust: '/presets/rust.svg',
    go: '/presets/go.svg',
    nixpacks: '/presets/nixpacks.svg',
  }
  return icons[preset.toLowerCase()] || '/presets/default.svg'
}

export function TemplateCard({ template, onClick, selected }: TemplateCardProps) {
  return (
    <Card
      className={cn(
        'cursor-pointer transition-all hover:border-primary/50 hover:shadow-md',
        selected && 'border-primary ring-2 ring-primary/20'
      )}
      onClick={() => onClick?.(template)}
    >
      <CardHeader className="pb-2">
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-3">
            <div className="h-10 w-10 rounded-md bg-muted flex items-center justify-center overflow-hidden">
              {template.image_url ? (
                <img
                  src={template.image_url}
                  alt={template.name}
                  className="h-8 w-8 object-contain"
                  onError={(e) => {
                    // Fallback to preset icon on error
                    e.currentTarget.src = getPresetIcon(template.preset)
                  }}
                />
              ) : (
                <img
                  src={getPresetIcon(template.preset)}
                  alt={template.preset}
                  className="h-6 w-6 object-contain"
                  onError={(e) => {
                    // Final fallback to server icon
                    e.currentTarget.style.display = 'none'
                    e.currentTarget.parentElement?.classList.add('text-muted-foreground')
                  }}
                />
              )}
            </div>
            <div>
              <CardTitle className="text-base font-semibold flex items-center gap-2">
                {template.name}
                {template.is_featured && (
                  <Star className="h-4 w-4 text-yellow-500 fill-yellow-500" />
                )}
              </CardTitle>
              <p className="text-xs text-muted-foreground">
                {template.preset}
              </p>
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
            <span>
              {template.services.map(getServiceLabel).join(', ')}
            </span>
          </div>
        )}

        {/* Features preview */}
        {template.features.length > 0 && (
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <Server className="h-3 w-3" />
            <span className="line-clamp-1">
              {template.features.slice(0, 2).join(' · ')}
              {template.features.length > 2 && ` +${template.features.length - 2}`}
            </span>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
