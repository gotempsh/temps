// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useId, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { GlobalLogLine } from '@/api/client/types.gen'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import { Search, X } from 'lucide-react'
import {
  LOG_FILTER_KEYS,
  LOG_FILTER_PARAMS,
  parseLogQuery,
  quoteLogValue,
} from '@/lib/log-query'

export function LogQueryInput({
  params,
  text,
  lines,
  onChange,
}: {
  params: URLSearchParams
  text: string
  lines: GlobalLogLine[]
  onChange: (patch: Record<string, string | undefined>) => void
}) {
  const input = useRef<HTMLInputElement>(null)
  const id = useId()
  const [draft, setDraft] = useState(text)
  const [open, setOpen] = useState(false)
  const [selected, setSelected] = useState(0)
  const [error, setError] = useState('')
  const [previousText, setPreviousText] = useState(text)
  if (previousText !== text) {
    setPreviousText(text)
    setDraft(text)
  }
  useEffect(() => {
    const focus = (event: KeyboardEvent) => {
      if (
        event.key === '/' &&
        !event.metaKey &&
        !event.ctrlKey &&
        !event.altKey &&
        !(
          event.target instanceof HTMLElement &&
          event.target.closest('input,textarea,select,[contenteditable="true"]')
        )
      ) {
        event.preventDefault()
        input.current?.focus()
      }
    }
    window.addEventListener('keydown', focus)
    return () => window.removeEventListener('keydown', focus)
  }, [])
  const tail =
    draft.match(
      /(?:^|\s)((?:project|source|level|env|node|deployment):(?:"[^" ]*(?: [^"]*)?"?|\S*)|\S*)$/i
    )?.[1] ?? ''
  const colon = tail.indexOf(':')
  const key = colon < 0 ? '' : tail.slice(0, colon).toLowerCase()
  const needle = (colon < 0 ? tail : tail.slice(colon + 1))
    .replace(/^"|"$/g, '')
    .toLowerCase()
  const projects = useQuery(
    getProjectsOptions({
      query: {
        per_page: 100,
        page: 1,
        search: key === 'project' ? needle || undefined : undefined,
      },
    })
  )
  const choices = projects.data?.projects ?? []
  const values: Record<string, { value: string; label: string }[]> = {
    project: choices.map((p) => ({ value: String(p.id), label: p.name })),
    source: [
      { value: 'collected', label: 'All collected logs' },
      { value: 'application', label: 'Applications' },
      { value: 'service', label: 'Databases' },
    ],
    level: ['error', 'warn', 'info', 'debug', 'trace'].map((value) => ({
      value,
      label: value,
    })),
    env: [...new Set(lines.map((l) => l.env).filter(Boolean))].map((value) => ({
      value,
      label: value,
    })),
    node: [
      ...new Map(
        lines
          .filter((l) => l.node_id != null)
          .map((l) => [
            l.node_id,
            {
              value: String(l.node_id),
              label: l.node_name || String(l.node_id),
            },
          ])
      ).values(),
    ],
    deployment: [
      ...new Set(lines.map((l) => l.deploy_id).filter((v) => v != null)),
    ].map((value) => ({ value: String(value), label: String(value) })),
  }
  const options =
    colon < 0
      ? LOG_FILTER_KEYS.filter((k) => k.startsWith(needle)).map((k) => ({
          value: `${k}:`,
          label: `${k}:`,
          hint: 'Choose a value',
          keyOnly: true,
        }))
      : (values[key] ?? [])
          .filter((v) => `${v.value} ${v.label}`.toLowerCase().includes(needle))
          .map((v) => ({
            value: `${key}:${quoteLogValue(v.value)}`,
            label: `${key}:${quoteLogValue(key === 'project' ? v.label : v.value)}`,
            hint: v.label === v.value ? '' : v.label,
            keyOnly: false,
          }))
  const activeIndex = Math.min(selected, options.length - 1)
  useEffect(() => {
    if (open && activeIndex >= 0)
      document
        .getElementById(`${id}-${activeIndex}`)
        ?.scrollIntoView({ block: 'nearest' })
  }, [activeIndex, open, id])
  const apply = (value: string) => {
    const result = parseLogQuery(value, choices)
    if (result.error !== undefined) {
      setError(result.error)
      return
    }
    onChange(result.patch)
    setDraft(result.patch.q ?? '')
    setError('')
    setOpen(false)
  }
  const choose = (option: (typeof options)[number]) => {
    const next = draft.slice(0, draft.length - tail.length) + option.value
    if (option.keyOnly) {
      setDraft(next)
      setSelected(0)
      setOpen(true)
    } else apply(next)
    input.current?.focus()
  }
  const tokens = LOG_FILTER_KEYS.filter((k) =>
    params.has(LOG_FILTER_PARAMS[k])
  ).map((k) => ({
    key: k,
    param: LOG_FILTER_PARAMS[k],
    value: params.get(LOG_FILTER_PARAMS[k])!,
  }))
  return (
    <div
      className="relative min-w-0"
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) setOpen(false)
      }}
    >
      <div className="flex min-h-10 flex-wrap items-center gap-1 rounded-md border bg-background px-2 focus-within:ring-1 focus-within:ring-ring">
        <Search className="mx-1 size-4 shrink-0 text-muted-foreground" />
        {tokens.map((token) => (
          <div
            key={token.key}
            className="flex max-w-full items-center rounded bg-secondary font-mono text-[11px]"
          >
            <button
              type="button"
              className="truncate py-1 pl-2 hover:underline"
              aria-label={`Edit ${token.key} filter`}
              onClick={() => {
                setDraft(
                  `${text ? `${text} ` : ''}${token.key}:${quoteLogValue(token.value)}`
                )
                setOpen(true)
                setError('')
                input.current?.focus()
              }}
            >
              {token.key}:
              {token.key === 'project'
                ? (choices.find((p) => String(p.id) === token.value)?.name ??
                  token.value)
                : token.key === 'level'
                  ? token.value.toLowerCase()
                  : token.value}
            </button>
            <Button
              variant="ghost"
              size="icon"
              className="size-6 shrink-0"
              aria-label={`Clear ${token.key === 'env' ? 'environment' : token.key}: ${token.value}`}
              onClick={() => onChange({ [token.param]: undefined })}
            >
              <X className="size-3" />
            </Button>
          </div>
        ))}
        <Input
          ref={input}
          role="combobox"
          aria-label="Search log messages"
          aria-autocomplete="list"
          aria-expanded={open}
          aria-controls={open ? `${id}-list` : undefined}
          aria-activedescendant={
            open && activeIndex >= 0 ? `${id}-${activeIndex}` : undefined
          }
          aria-invalid={!!error}
          aria-describedby={error ? `${id}-error` : undefined}
          value={draft}
          className="h-9 min-w-36 flex-1 border-0 bg-transparent px-1 font-mono text-xs shadow-none focus-visible:outline-none focus-visible:ring-0"
          placeholder="Search messages or add key:value…"
          onFocus={() => setOpen(true)}
          onChange={(event) => {
            const value = event.target.value
            setDraft(value)
            setOpen(true)
            setSelected(0)
            setError('')
            if (
              !/(?:^|\s)(project|source|level|env|node|deployment):/i.test(
                value
              )
            )
              onChange({ q: value || undefined })
          }}
          onKeyDown={(event) => {
            if (event.nativeEvent.isComposing) return
            if (event.key === 'Escape') {
              event.preventDefault()
              setOpen(false)
              setError('')
              setDraft(text)
            } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
              event.preventDefault()
              setOpen(true)
              setSelected((i) =>
                options.length
                  ? (i +
                      (event.key === 'ArrowDown' ? 1 : -1) +
                      options.length) %
                    options.length
                  : 0
              )
            } else if (
              event.key === 'Enter' ||
              (event.key === 'Tab' && open && options.length > 0)
            ) {
              event.preventDefault()
              if (open && options[activeIndex]) choose(options[activeIndex])
              else apply(draft)
            }
          }}
        />
        {(draft || text) && (
          <Button
            variant="ghost"
            size="icon"
            className="size-6"
            aria-label="Clear search"
            onClick={() => {
              setDraft('')
              setError('')
              onChange({ q: undefined })
            }}
          >
            <X className="size-3" />
          </Button>
        )}
      </div>
      {error && (
        <p
          role="alert"
          id={`${id}-error`}
          className="mt-1 text-xs text-destructive"
        >
          {error}
        </p>
      )}
      {open && (
        <div className="absolute inset-x-0 top-full z-30 mt-1 rounded-md border bg-popover text-popover-foreground shadow-md">
          <div className="border-b px-3 py-2 text-[11px] text-muted-foreground">
            {colon < 0
              ? 'Filters · type key:value or search message text'
              : `Choose ${key} · Enter to apply`}
          </div>
          <div
            role="listbox"
            id={`${id}-list`}
            aria-label="Log filter suggestions"
            className="max-h-60 overflow-y-auto p-1"
          >
            {options.map((option, index) => (
              <div
                key={option.value}
                role="option"
                id={`${id}-${index}`}
                aria-selected={index === activeIndex}
                className={`flex cursor-pointer items-center justify-between gap-3 rounded px-2 py-2 text-xs ${index === activeIndex ? 'bg-accent text-accent-foreground' : ''}`}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => choose(option)}
              >
                <span className="truncate font-mono">{option.label}</span>
                <span className="truncate text-muted-foreground">
                  {option.hint}
                </span>
              </div>
            ))}
            {!options.length && (
              <p className="px-2 py-3 text-xs text-muted-foreground">
                {key === 'project' && projects.isFetching
                  ? 'Loading projects…'
                  : colon >= 0
                    ? 'No matching suggestions. Enter a value and press Enter.'
                    : 'Press Enter to search messages.'}
              </p>
            )}
          </div>
          {key === 'project' && projects.isError && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void projects.refetch()}
            >
              Retry project suggestions
            </Button>
          )}
          {key === 'project' && (projects.data?.total ?? 0) > 100 && (
            <p className="px-3 pb-2 text-xs text-muted-foreground">
              Type a project name to narrow suggestions.
            </p>
          )}
          {['env', 'node', 'deployment'].includes(key) && (
            <p className="border-t px-3 py-2 text-[11px] text-muted-foreground">
              Suggestions from loaded logs. You can also enter{' '}
              {key === 'env' ? 'an environment' : 'an ID'}.
            </p>
          )}
        </div>
      )}
    </div>
  )
}
