import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { Separator } from '@/components/ui/separator'
import { formatUTCDate } from '@/lib/date'
import { Copy, CopyCheck, RefreshCw } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { DnsTxtRecord } from './DnsTxtRecordsDisplay'

interface AcmeOrderInfoProps {
  order: {
    id: number
    status: string
    email: string
    expires_at?: number | null
    created_at: number
    updated_at: number
    error?: string | null
    error_type?: string | null
  }
  dnsTxtRecords?: DnsTxtRecord[]
  onRefresh?: () => void
  showRefresh?: boolean
}

const getStatusBadgeVariant = (status: string) => {
  switch (status) {
    case 'active':
    case 'valid':
      return 'default'
    case 'pending':
    case 'processing':
      return 'secondary'
    case 'failed':
    case 'invalid':
      return 'destructive'
    default:
      return 'outline'
  }
}

export function AcmeOrderInfo({
  order,
  dnsTxtRecords,
  onRefresh,
  showRefresh = true,
}: AcmeOrderInfoProps) {
  const [copiedField, setCopiedField] = useState<string | null>(null)

  const handleCopy = (text: string, field: string) => {
    navigator.clipboard.writeText(text)
    setCopiedField(field)
    toast.success('Copied to clipboard')
    setTimeout(() => setCopiedField(null), 2000)
  }

  return (
    <Card>
      <div className="p-6 space-y-4">
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-semibold">ACME Order</h2>
          {showRefresh && onRefresh && (
            <Button size="sm" variant="outline" onClick={onRefresh}>
              <RefreshCw className="mr-2 h-4 w-4" />
              Refresh
            </Button>
          )}
        </div>
        <div className="space-y-3">
          <div className="space-y-1">
            <span className="text-xs text-muted-foreground">Order Status</span>
            <div>
              <Badge variant={getStatusBadgeVariant(order.status)}>
                {order.status}
              </Badge>
            </div>
          </div>
          <div className="space-y-1">
            <span className="text-xs text-muted-foreground">Order ID</span>
            <p className="text-sm font-mono">#{order.id}</p>
          </div>
          <div className="space-y-1">
            <span className="text-xs text-muted-foreground">Email</span>
            <p className="text-sm">{order.email}</p>
          </div>
          {order.expires_at && (
            <div className="space-y-1">
              <span className="text-xs text-muted-foreground">
                Order Expires
              </span>
              <p className="text-sm">{formatUTCDate(order.expires_at)}</p>
            </div>
          )}
          {dnsTxtRecords && dnsTxtRecords.length > 0 && (
            <>
              <Separator />
              <div className="space-y-2">
                <span className="text-xs text-muted-foreground">
                  DNS Challenge
                </span>
                <div className="space-y-3">
                  {dnsTxtRecords.map((record, index) => (
                    <div
                      key={index}
                      className="space-y-2 p-3 bg-muted/50 rounded-md"
                    >
                      {dnsTxtRecords.length > 1 && (
                        <div className="flex items-center justify-between">
                          <span className="text-xs font-medium">
                            Record {index + 1}
                          </span>
                          <Badge variant="outline" className="text-xs">
                            TXT
                          </Badge>
                        </div>
                      )}
                      <div className="space-y-1">
                        <span className="text-xs font-medium">Name:</span>
                        <div className="flex items-center gap-2">
                          <code className="text-xs font-mono break-all flex-1">
                            {record.name}
                          </code>
                          <Button
                            size="sm"
                            variant="ghost"
                            className="h-6 w-6 p-0"
                            onClick={() =>
                              handleCopy(record.name, `sidebar-name-${index}`)
                            }
                          >
                            {copiedField === `sidebar-name-${index}` ? (
                              <CopyCheck className="h-3 w-3" />
                            ) : (
                              <Copy className="h-3 w-3" />
                            )}
                          </Button>
                        </div>
                      </div>
                      <div className="space-y-1">
                        <span className="text-xs font-medium">Value:</span>
                        <div className="flex items-center gap-2">
                          <code className="text-xs font-mono break-all flex-1">
                            {record.value}
                          </code>
                          <Button
                            size="sm"
                            variant="ghost"
                            className="h-6 w-6 p-0"
                            onClick={() =>
                              handleCopy(record.value, `sidebar-value-${index}`)
                            }
                          >
                            {copiedField === `sidebar-value-${index}` ? (
                              <CopyCheck className="h-3 w-3" />
                            ) : (
                              <Copy className="h-3 w-3" />
                            )}
                          </Button>
                        </div>
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            </>
          )}
          {order.error && (
            <>
              <Separator />
              <div className="space-y-1">
                <span className="text-xs text-muted-foreground text-destructive">
                  Error
                </span>
                <p className="text-sm text-destructive">{order.error_type}</p>
                <p className="text-xs text-muted-foreground">{order.error}</p>
              </div>
            </>
          )}
          <Separator />
          <div className="space-y-1">
            <span className="text-xs text-muted-foreground">Created</span>
            <p className="text-sm">{formatUTCDate(order.created_at)}</p>
          </div>
          <div className="space-y-1">
            <span className="text-xs text-muted-foreground">Updated</span>
            <p className="text-sm">{formatUTCDate(order.updated_at)}</p>
          </div>
        </div>
      </div>
    </Card>
  )
}
