import { cn } from '@/lib/utils'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'

interface StatusIndicatorProps {
  status: 'success' | 'failure' | 'running' | 'pending' | 'cancelled'
}

export function StatusIndicator({ status }: StatusIndicatorProps) {
  const statusLabels = {
    success: 'Success',
    failure: 'Failed',
    running: 'Running',
    pending: 'Pending',
    cancelled: 'Cancelled',
  }

  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>
          <div
            className={cn(
              'w-3 h-3 rounded-full cursor-help',
              status === 'success' && 'bg-green-500',
              status === 'failure' && 'bg-red-500',
              status === 'running' && 'bg-yellow-500',
              status === 'pending' && 'bg-gray-500',
              status === 'cancelled' && 'bg-gray-400'
            )}
          />
        </TooltipTrigger>
        <TooltipContent>
          <p className="text-xs capitalize">{statusLabels[status]}</p>
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  )
}
