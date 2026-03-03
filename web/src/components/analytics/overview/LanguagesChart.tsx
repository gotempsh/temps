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
import { Languages } from 'lucide-react'
import * as React from 'react'

// Map common language codes to human-readable names
const LANGUAGE_NAMES: Record<string, string> = {
  en: 'English',
  'en-US': 'English (US)',
  'en-GB': 'English (UK)',
  'en-AU': 'English (Australia)',
  'en-CA': 'English (Canada)',
  es: 'Spanish',
  'es-ES': 'Spanish (Spain)',
  'es-MX': 'Spanish (Mexico)',
  'es-AR': 'Spanish (Argentina)',
  fr: 'French',
  'fr-FR': 'French (France)',
  'fr-CA': 'French (Canada)',
  de: 'German',
  'de-DE': 'German (Germany)',
  'de-AT': 'German (Austria)',
  it: 'Italian',
  pt: 'Portuguese',
  'pt-BR': 'Portuguese (Brazil)',
  'pt-PT': 'Portuguese (Portugal)',
  nl: 'Dutch',
  ru: 'Russian',
  ja: 'Japanese',
  ko: 'Korean',
  zh: 'Chinese',
  'zh-CN': 'Chinese (Simplified)',
  'zh-TW': 'Chinese (Traditional)',
  ar: 'Arabic',
  hi: 'Hindi',
  tr: 'Turkish',
  pl: 'Polish',
  sv: 'Swedish',
  da: 'Danish',
  fi: 'Finnish',
  no: 'Norwegian',
  nb: 'Norwegian',
  cs: 'Czech',
  el: 'Greek',
  he: 'Hebrew',
  th: 'Thai',
  vi: 'Vietnamese',
  id: 'Indonesian',
  ms: 'Malay',
  uk: 'Ukrainian',
  ro: 'Romanian',
  hu: 'Hungarian',
  bg: 'Bulgarian',
  hr: 'Croatian',
  sk: 'Slovak',
  sl: 'Slovenian',
  lt: 'Lithuanian',
  lv: 'Latvian',
  et: 'Estonian',
  ca: 'Catalan',
  eu: 'Basque',
  gl: 'Galician',
}

function getLanguageName(code: string): string {
  if (!code) return 'Unknown'
  // Try exact match first
  if (LANGUAGE_NAMES[code]) return LANGUAGE_NAMES[code]
  // Try base language code (e.g., "en" from "en-US")
  const base = code.split('-')[0]
  if (LANGUAGE_NAMES[base]) return `${LANGUAGE_NAMES[base]} (${code})`
  return code
}

interface LanguagesChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function LanguagesChart({
  project,
  startDate,
  endDate,
  environment,
}: LanguagesChartProps) {
  const { data, isLoading, error } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: 'language',
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 10,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const sortedLanguages = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
      .sort((a, b) => b.count - a.count)
      .map((item) => ({
        code: item.value || 'Unknown',
        name: getLanguageName(item.value || ''),
        count: item.count,
        percentage: ((item.count / total) * 100).toFixed(1),
      }))
  }, [data])

  return (
    <Card>
      <CardHeader>
        <CardTitle>Languages</CardTitle>
        <CardDescription>
          {startDate && endDate
            ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
            : 'Select a date range'}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            {[...Array(5)].map((_, i) => (
              <div key={`skeleton-${i}`} className="flex items-center justify-between">
                <div className="h-4 w-[150px] bg-muted animate-pulse rounded" />
                <div className="h-4 w-[100px] bg-muted animate-pulse rounded" />
              </div>
            ))}
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load language analytics
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !sortedLanguages.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No data available for the selected period
            </p>
          </div>
        ) : (
          <div className="space-y-3" style={{ minHeight: '400px' }}>
            {sortedLanguages.map((lang) => (
              <div key={lang.code} className="space-y-2">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-3">
                    <Languages className="h-4 w-4 text-muted-foreground" />
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium">{lang.name}</span>
                      {lang.code !== lang.name && (
                        <span className="text-xs text-muted-foreground">
                          {lang.code}
                        </span>
                      )}
                    </div>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className="text-sm text-muted-foreground">
                      {lang.percentage}%
                    </span>
                    <span className="text-sm font-mono text-muted-foreground">
                      {lang.count.toLocaleString()}
                    </span>
                  </div>
                </div>
                <div className="relative h-2 bg-muted rounded-full overflow-hidden">
                  <div
                    className="absolute inset-y-0 left-0 bg-primary rounded-full transition-all duration-500"
                    style={{ width: `${lang.percentage}%` }}
                  />
                </div>
              </div>
            ))}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && sortedLanguages.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {sortedLanguages.length} languages by unique visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}
