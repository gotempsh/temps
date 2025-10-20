import { Badge } from '@/components/ui/badge'
import { DeploymentStatus } from './DeploymentStatus'
import { DeploymentResponse } from '@/api/client'

const statusStyles = {
  completed: 'bg-emerald-50 text-emerald-700 border-emerald-200',
  failed: 'bg-red-50 text-red-700 border-red-200',
  pending: 'bg-yellow-50 text-yellow-700 border-yellow-200',
  running: 'bg-blue-50 text-blue-700 border-blue-200',
}

interface DeploymentStatusBadgeProps {
  deployment: DeploymentResponse
  className?: string
}

export function DeploymentStatusBadge({
  deployment,
  className,
}: DeploymentStatusBadgeProps) {
  return (
    <Badge
      variant="outline"
      className={`${statusStyles[deployment.status as keyof typeof statusStyles]} ${className}`}
    >
      <DeploymentStatus deployment={deployment} />
    </Badge>
  )
}
