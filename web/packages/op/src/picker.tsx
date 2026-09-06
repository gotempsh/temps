// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState, type ReactNode } from 'react'
import { Check, ChevronsUpDown, RotateCcw } from 'lucide-react'
import { cn } from './lib/cn'
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from './ui/command'
import { Popover, PopoverContent, PopoverTrigger } from './ui/popover'
import { Kbd } from './kbd'
import { GLYPH, GLYPH_CLASS, type State } from './status'

/**
 * Picker: the searchable select. Anything with more than ~7 options, or
 * options the operator has to recognise rather than recall (branches, images,
 * regions, environments, providers), is a Picker, never a plain <select>.
 *
 *  - the trigger is a mono field, same height and border as an Input, with
 *    the current value only (meta lives in the list); a placeholder when
 *    nothing is chosen
 *  - opens to a search box (autofocused, `/` is not needed) and grouped rows;
 *    type to filter, ↑↓ to move, ⏎ to choose, Esc to close
 *  - each row: optional state glyph, label, and a muted `meta` on the right
 *    (last commit, "default", region). The current value is marked ●
 *  - `allowCustom`: typing something not in the list offers "use <typed>",
 *    for branches that do not exist yet or values the list did not load
 *  - `loading` and `error` are real states inside the list, not a spinner
 *    on the trigger: the operator sees what was being fetched and can retry
 *  - `skin` is applied to the portal content, like EchoDialog
 */
export type PickerOption = {
  value: string
  label?: string
  group?: string
  meta?: ReactNode
  state?: State
  /** A small icon that describes the option (permission mode, provider). Drawn in the glyph slot, coloured by `state` if any. */
  icon?: ReactNode
  /** Extra words that should match the filter (issue id, sha, alias). */
  keywords?: string
  disabled?: boolean
}

export function Picker({ value, onChange, options, label, placeholder = 'choose…', searchPlaceholder = 'type to filter', allowCustom, loading, error, onRetry, skin = 'operator ink v4 v5', className, mono = true, width }: {
  value: string | null | undefined
  onChange: (v: string) => void
  options: PickerOption[]
  /** What the field is ("auto-deploy branch"); spoken as the combobox's name. Falls back to the placeholder. */
  label?: string
  placeholder?: string
  searchPlaceholder?: string
  /** Label for the custom-value row, e.g. "use branch". Enables custom values. */
  allowCustom?: string
  /** What is being loaded, e.g. "branches from github.com/acme/web". */
  loading?: string | false
  /** Why the list failed, verbatim from the source. */
  error?: string | false
  onRetry?: () => void
  skin?: string
  className?: string
  mono?: boolean
  /** Popover width; defaults to the trigger width, at least 380px, capped to the viewport. */
  width?: string
}) {
  const [open, setOpen] = useState(false)
  const [q, setQ] = useState('')
  const selected = options.find((o) => o.value === value)
  const groups = useMemo(() => {
    const m = new Map<string, PickerOption[]>()
    for (const o of options) { const k = o.group ?? ''; m.set(k, [...(m.get(k) ?? []), o]) }
    return [...m.entries()]
  }, [options])
  const typed = q.trim()
  const custom = allowCustom && typed && !options.some((o) => o.value === typed)
  const choose = (v: string) => { onChange(v); setOpen(false); setQ('') }

  return (
    <Popover open={open} onOpenChange={(o) => { setOpen(o); if (!o) setQ('') }}>
      <PopoverTrigger asChild>
        <button
          type="button"
          role="combobox"
          aria-expanded={open}
          // A combobox takes no name from its contents (ARIA), so the field's name is always spoken: `label` when given, else the placeholder.
          aria-label={label ?? placeholder}
          className={cn('flex h-8 w-full items-center gap-2 border bg-background px-2 text-left text-xs hover:bg-muted focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring', mono && 'font-mono', !selected && !value && 'text-muted-foreground', className)}
        >
          {selected?.icon ? <span aria-hidden className={cn('flex w-3.5 shrink-0 items-center justify-center [&_svg]:h-3.5 [&_svg]:w-3.5', selected.state && GLYPH_CLASS[selected.state])}>{selected.icon}</span>
            : selected?.state && <span aria-hidden className={cn('w-3 shrink-0 text-center', GLYPH_CLASS[selected.state])}>{GLYPH[selected.state]}</span>}
          <span className="min-w-0 flex-1 truncate">{selected?.label ?? value ?? placeholder}</span>
          <ChevronsUpDown className="h-3.5 w-3.5 shrink-0 opacity-50" />
        </button>
      </PopoverTrigger>
      <PopoverContent align="start" className={cn(skin, 'border p-0 shadow-none sm:rounded')} style={{ width: width ?? 'min(calc(100vw - 2rem), max(380px, var(--radix-popover-trigger-width)))' }}>
        <Command className={cn('rounded-none bg-popover', mono && 'font-mono')}>
          <CommandInput value={q} onValueChange={setQ} placeholder={searchPlaceholder} className="h-8 text-xs" />
          <CommandList className="max-h-72 text-xs">
            {loading && <p className="flex items-center gap-2 px-3 py-3 text-muted-foreground"><span aria-hidden>◌</span> loading {loading}</p>}
            {error && (
              <div className="space-y-2 px-3 py-3">
                <p className="flex items-start gap-2"><span aria-hidden className="text-destructive">×</span><span className="min-w-0 break-words">{error}</span></p>
                {onRetry && <button type="button" onClick={onRetry} className="inline-flex h-7 items-center gap-1 border px-2 hover:bg-muted"><RotateCcw className="h-3 w-3" /> retry</button>}
                {allowCustom && <p className="text-[11px] text-muted-foreground">or type a value above and choose &quot;{allowCustom}&quot;.</p>}
              </div>
            )}
            {!loading && !error && <CommandEmpty className="px-3 py-3 text-left text-muted-foreground">{custom ? null : `nothing matches "${typed}"`}</CommandEmpty>}
            {!loading && !error && groups.map(([g, items]) => (
              <CommandGroup key={g || '__'} heading={g || undefined} className="[&_[cmdk-group-heading]]:py-1 [&_[cmdk-group-heading]]:text-[10px] [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:uppercase [&_[cmdk-group-heading]]:tracking-[0.1em]">
                {items.map((o) => (
                  <CommandItem key={o.value} value={`${o.label ?? o.value} ${o.value} ${o.keywords ?? ''}`} disabled={o.disabled} onSelect={() => choose(o.value)} className="gap-2 rounded-none data-[selected=true]:bg-foreground data-[selected=true]:text-background">
                    {o.icon
                      ? <span aria-hidden className={cn('flex w-3.5 shrink-0 items-center justify-center [&_svg]:h-3.5 [&_svg]:w-3.5', o.value !== value && o.state && GLYPH_CLASS[o.state])}>{o.icon}</span>
                      : <span aria-hidden className={cn('w-3 shrink-0 text-center', o.value === value ? '' : o.state ? GLYPH_CLASS[o.state] : 'opacity-0')}>{o.value === value ? '●' : o.state ? GLYPH[o.state] : '○'}</span>}
                    <span className={cn('shrink-0', o.value === value && 'font-medium')}>{o.label ?? o.value}</span>
                    {o.meta && <span className="min-w-0 flex-1 truncate text-right text-[11px] opacity-60" title={typeof o.meta === 'string' ? o.meta : undefined}>{o.meta}</span>}
                    {o.icon && o.value === value && <Check aria-label="selected" className="h-3.5 w-3.5 shrink-0" />}
                  </CommandItem>
                ))}
              </CommandGroup>
            ))}
            {custom && (
              <CommandGroup forceMount className="[&_[cmdk-group-heading]]:hidden">
                <CommandItem value={`__custom ${typed}`} onSelect={() => choose(typed)} className="rounded-none data-[selected=true]:bg-foreground data-[selected=true]:text-background">
                  <span className="text-muted-foreground group-data-[selected=true]:text-inherit">{allowCustom}</span> <span className="truncate">{typed}</span>
                </CommandItem>
              </CommandGroup>
            )}
          </CommandList>
          <div className="flex items-center gap-2 whitespace-nowrap border-t px-3 py-1.5 text-[10px] text-muted-foreground">
            <Kbd keys="↑↓" /> move <Kbd keys="⏎" className="ml-1" /> choose <Kbd keys="esc" className="ml-1" /> close
          </div>
        </Command>
      </PopoverContent>
    </Popover>
  )
}
