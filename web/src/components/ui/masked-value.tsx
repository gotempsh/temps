import { Eye, EyeOff } from 'lucide-react'
import { useState } from 'react'
import { Button } from './button'
import { maskValue } from '@/lib/masking'

interface MaskedValueProps {
  value: string
  className?: string
}

export function MaskedValue({ value, className = '' }: MaskedValueProps) {
  const [isVisible, setIsVisible] = useState(false)

  return (
    <div className={`flex items-center gap-2 ${className}`}>
      <code className="px-2 py-1 bg-muted rounded text-xs break-all flex-1 font-mono">
        {isVisible ? value : maskValue(value)}
      </code>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="h-7 w-7 flex-shrink-0"
        onClick={() => setIsVisible(!isVisible)}
      >
        {isVisible ? (
          <EyeOff className="h-3.5 w-3.5" />
        ) : (
          <Eye className="h-3.5 w-3.5" />
        )}
      </Button>
    </div>
  )
}
