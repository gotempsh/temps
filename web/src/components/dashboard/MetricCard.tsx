import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'

interface MetricCardProps {
  title: string
  value: string | number
  change: string
  icon: React.ReactNode
  changeDisplay?: {
    icon: React.ReactNode
    className: string
    isPositive?: boolean
  }
  error?: boolean
}

export function MetricCard({
  title,
  value,
  change,
  icon,
  changeDisplay,
  error,
}: MetricCardProps) {
  return (
    <Card className={`${error ? 'border-destructive/50' : ''} h-full w-full`}>
      <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
        <CardTitle className="text-sm font-medium">{title}</CardTitle>
        {icon}
      </CardHeader>
      <CardContent>
        <div className="text-2xl font-bold">{value}</div>
        {changeDisplay ? (
          <p className={changeDisplay.className}>
            {changeDisplay.icon}
            {change}
          </p>
        ) : (
          <p className="text-xs text-muted-foreground">{change}</p>
        )}
      </CardContent>
    </Card>
  )
}
