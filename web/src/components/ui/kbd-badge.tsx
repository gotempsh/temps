import { cn } from '@/lib/utils'

interface KbdBadgeProps {
  /**
   * The key(s) to display (e.g., 'N', 'Ctrl+K')
   */
  keys: string | string[]
  /**
   * Additional CSS classes
   */
  className?: string
  /**
   * Whether to show on mobile (default: false)
   */
  showOnMobile?: boolean
}

/**
 * Display keyboard shortcut key(s) in a styled badge format
 * @example
 * <KbdBadge keys="N" />
 * <KbdBadge keys={['⌘', 'K']} />
 */
export function KbdBadge({
  keys,
  className,
  showOnMobile = false,
}: KbdBadgeProps) {
  const keyArray = Array.isArray(keys) ? keys : [keys]

  return (
    <span
      className={cn(
        'inline-flex items-center gap-1',
        showOnMobile ? 'flex' : 'hidden sm:flex',
        className
      )}
    >
      {keyArray.map((key, index) => (
        <kbd
          key={index}
          className="pointer-events-none inline-flex h-5 select-none items-center gap-1 rounded border bg-muted px-1.5 font-mono text-[10px] font-medium text-muted-foreground opacity-100"
        >
          {key}
        </kbd>
      ))}
    </span>
  )
}
