import { listDomainsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { cn } from '@/lib/utils'
import { useDebounce } from '@/hooks/useDebounce'
import { useQuery } from '@tanstack/react-query'
import { Check, ChevronsUpDown, Globe } from 'lucide-react'
import { useState } from 'react'

interface DomainSelectorProps {
  /** Currently selected domain string (e.g. "example.com" or "*.example.com") */
  value: string
  /** Called when user selects a domain */
  onValueChange: (value: string) => void
  /** Placeholder text when no domain is selected */
  placeholder?: string
  /** Additional class names for the trigger button */
  className?: string
  /** Whether the selector is disabled */
  disabled?: boolean
  /** Width of the popover content */
  popoverWidth?: string
  /** Align the popover */
  align?: 'start' | 'center' | 'end'
}

export function DomainSelector({
  value,
  onValueChange,
  placeholder = 'Select domain...',
  className,
  disabled = false,
  popoverWidth = 'w-[350px]',
  align = 'start',
}: DomainSelectorProps) {
  const [open, setOpen] = useState(false)
  const [search, setSearch] = useState('')
  const debouncedSearch = useDebounce(search, 200)

  const { data, isLoading } = useQuery({
    ...listDomainsOptions({
      query: {
        search: debouncedSearch || undefined,
        page_size: 50,
      },
    }),
    enabled: open,
  })

  const domains = data?.domains ?? []

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          variant="outline"
          role="combobox"
          aria-expanded={open}
          disabled={disabled}
          className={cn(
            'justify-between font-normal',
            !value && 'text-muted-foreground',
            className
          )}
        >
          <span className="truncate">
            {value || placeholder}
          </span>
          <ChevronsUpDown className="ml-2 h-4 w-4 shrink-0 opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent className={cn(popoverWidth, 'p-0')} align={align}>
        <Command shouldFilter={false}>
          <CommandInput
            placeholder="Search domains..."
            value={search}
            onValueChange={setSearch}
          />
          <CommandList>
            <CommandEmpty>
              {isLoading ? (
                <span className="text-muted-foreground">Loading...</span>
              ) : (
                <span className="text-muted-foreground">No domains found.</span>
              )}
            </CommandEmpty>
            {domains.length > 0 && (
              <CommandGroup>
                {domains.map((domain) => (
                  <CommandItem
                    key={domain.id}
                    value={domain.domain}
                    onSelect={() => {
                      onValueChange(domain.domain)
                      setOpen(false)
                      setSearch('')
                    }}
                  >
                    <Check
                      className={cn(
                        'mr-2 h-4 w-4 shrink-0',
                        value === domain.domain ? 'opacity-100' : 'opacity-0'
                      )}
                    />
                    <Globe className="mr-2 h-4 w-4 shrink-0 text-muted-foreground" />
                    <span className="truncate flex-1">{domain.domain}</span>
                    <DomainStatusBadge status={domain.status} />
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
            {data && data.total > domains.length && (
              <div className="px-2 py-1.5 text-center text-xs text-muted-foreground border-t">
                Showing {domains.length} of {data.total} — type to search
              </div>
            )}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}

function DomainStatusBadge({ status }: { status: string }) {
  const config: Record<string, { label: string; className: string }> = {
    active: {
      label: 'Active',
      className: 'bg-green-100 text-green-700 dark:bg-green-900 dark:text-green-300',
    },
    // Still serving a live cert, but the last renewal failed — warn, don't alarm.
    active_renewal_failed: {
      label: 'Renewal failed',
      className: 'bg-amber-100 text-amber-700 dark:bg-amber-900 dark:text-amber-300',
    },
    pending: {
      label: 'Pending',
      className: 'bg-yellow-100 text-yellow-700 dark:bg-yellow-900 dark:text-yellow-300',
    },
    pending_dns: {
      label: 'DNS',
      className: 'bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300',
    },
    failed: {
      label: 'Failed',
      className: 'bg-red-100 text-red-700 dark:bg-red-900 dark:text-red-300',
    },
  }

  const { label, className } = config[status] ?? {
    label: status,
    className: 'bg-muted text-muted-foreground',
  }

  return (
    <span className={cn('ml-2 shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium', className)}>
      {label}
    </span>
  )
}
