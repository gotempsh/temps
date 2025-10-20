import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Copy, CopyCheck, ExternalLink, Info } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'

export interface DnsTxtRecord {
  name: string
  value: string
}

interface DnsTxtRecordsDisplayProps {
  records: DnsTxtRecord[]
  showPropagationLinks?: boolean
  variant?: 'default' | 'compact'
}

export function DnsTxtRecordsDisplay({
  records,
  showPropagationLinks = true,
  variant = 'default',
}: DnsTxtRecordsDisplayProps) {
  const [copiedField, setCopiedField] = useState<string | null>(null)

  const handleCopy = (text: string, field: string) => {
    navigator.clipboard.writeText(text)
    setCopiedField(field)
    toast.success('Copied to clipboard')
    setTimeout(() => setCopiedField(null), 2000)
  }

  if (records.length === 0) {
    return null
  }

  return (
    <div className="space-y-4">
      {variant === 'default' && (
        <Alert>
          <Info className="h-4 w-4" />
          <AlertTitle>
            Add DNS TXT Record{records.length > 1 ? 's' : ''}
          </AlertTitle>
          <AlertDescription>
            Add the following TXT record{records.length > 1 ? 's' : ''} to your
            DNS provider:
          </AlertDescription>
        </Alert>
      )}

      <div className="space-y-4">
        {records.map((record, index) => (
          <div
            key={index}
            className="space-y-3 p-4 bg-muted/50 rounded-lg border"
          >
            {records.length > 1 && (
              <div className="flex items-center justify-between">
                <span className="text-sm font-medium">Record {index + 1}</span>
                <Badge variant="outline">TXT</Badge>
              </div>
            )}
            <div className="flex items-center justify-between gap-2">
              <div className="space-y-1 flex-1 min-w-0">
                <span className="text-xs font-medium text-muted-foreground">
                  Name
                </span>
                <p className="font-mono text-sm break-all">{record.name}</p>
              </div>
              <Button
                size="sm"
                variant="outline"
                onClick={() => handleCopy(record.name, `name-${index}`)}
              >
                {copiedField === `name-${index}` ? (
                  <CopyCheck className="h-4 w-4" />
                ) : (
                  <Copy className="h-4 w-4" />
                )}
              </Button>
            </div>
            <div className="flex items-center justify-between gap-2">
              <div className="space-y-1 flex-1 min-w-0">
                <span className="text-xs font-medium text-muted-foreground">
                  Value
                </span>
                <p className="font-mono text-sm break-all">{record.value}</p>
              </div>
              <Button
                size="sm"
                variant="outline"
                onClick={() => handleCopy(record.value, `value-${index}`)}
              >
                {copiedField === `value-${index}` ? (
                  <CopyCheck className="h-4 w-4" />
                ) : (
                  <Copy className="h-4 w-4" />
                )}
              </Button>
            </div>
          </div>
        ))}
      </div>

      {showPropagationLinks && variant === 'default' && (
        <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
          <Info className="h-4 w-4 text-blue-600" />
          <AlertDescription>
            <p className="mb-2 text-sm">
              After adding the TXT record{records.length > 1 ? 's' : ''}, wait
              for {records.length > 1 ? 'them' : 'it'} to propagate (usually
              5-15 minutes, up to 24 hours).
            </p>
            <p className="text-sm">
              Check propagation:
              {records.map((record, index) => (
                <span key={index}>
                  {index > 0 && ', '}
                  <a
                    href={`https://www.whatsmydns.net/#TXT/${record.name}`}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="underline inline-flex items-center gap-1"
                  >
                    {record.name} <ExternalLink className="h-3 w-3" />
                  </a>
                </span>
              ))}
            </p>
          </AlertDescription>
        </Alert>
      )}
    </div>
  )
}
