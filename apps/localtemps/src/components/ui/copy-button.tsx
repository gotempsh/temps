import * as React from 'react'
import { CheckIcon, CopyIcon } from 'lucide-react'

import { cn } from '@/lib/utils'
import { Button, type ButtonProps } from '@/components/ui/button'

interface CopyButtonProps extends ButtonProps {
  value: string
}

export function CopyButton({
  value,
  className,
  children,
  variant = 'outline',
  size = 'sm',
  ...props
}: CopyButtonProps) {
  const [hasCopied, setHasCopied] = React.useState(false)

  React.useEffect(() => {
    if (hasCopied) {
      const timeout = setTimeout(() => setHasCopied(false), 2000)
      return () => clearTimeout(timeout)
    }
  }, [hasCopied])

  const handleCopy = () => {
    navigator.clipboard.writeText(value)
    setHasCopied(true)
  }

  return (
    <Button
      variant={variant}
      size={size}
      className={cn('gap-2', className)}
      onClick={handleCopy}
      {...props}
    >
      {children}
      {hasCopied ? (
        <CheckIcon className="h-4 w-4 text-success" />
      ) : (
        <CopyIcon className="h-4 w-4" />
      )}
    </Button>
  )
}
