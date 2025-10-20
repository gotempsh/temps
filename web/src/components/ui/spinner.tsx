import { cn } from '@/lib/utils'
import { Loader2 } from 'lucide-react'

export function Spinner({ className }: { className?: string }) {
  return <Loader2 className={cn('h-4 w-4 animate-spin', className)} />
}
