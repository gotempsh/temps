// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { Link, type To } from 'react-router'
import { Menu, type LucideIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from '@/components/ui/sheet'
import { cn } from '@/lib/utils'
import { projectSectionLinkMatches } from '@/lib/project-section-search'
import { useUrlSearchText } from '@temps-sdk/ds'

export interface SectionNavLink {
  key: string
  title: string
  /** Extra terms the page finder matches besides the title and key. */
  keywords?: string[]
  to: To
  icon: LucideIcon
  active: boolean
}

/**
 * One flat contextual navigation beside a page's content: a sticky column on
 * large screens, a sheet behind a button on small ones. Shared by a service's
 * sections and a project's settings so both read as the same console.
 */
export function SectionNavLayout({
  title,
  links,
  children,
}: {
  title: string
  links: SectionNavLink[]
  children: ReactNode
}) {
  // `find` keeps the page finder across reload, back/forward and shared links.
  const [search, setSearch] = useUrlSearchText('find')
  const [open, setOpen] = useState(false)
  const visible = links.filter((link) =>
    projectSectionLinkMatches(
      { title: link.title, url: link.key, keywords: link.keywords },
      search
    )
  )
  const navigation = (
    <>
      <p className="mb-3 text-sm font-semibold">{title}</p>
      {links.length > 7 && (
        <Input
          aria-label={`Find ${title.toLowerCase()} page`}
          placeholder="Find a page…"
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          className="mb-2 h-8 text-xs"
        />
      )}
      <nav aria-label={`${title} pages`} className="space-y-0.5">
        {visible.map(({ key, title: label, to, icon: Icon, active }) => (
          <Link
            key={key}
            to={to}
            aria-current={active ? 'page' : undefined}
            onClick={() => {
              setOpen(false)
              setSearch('')
            }}
            className={cn(
              'flex items-center gap-2 rounded-md px-2.5 py-2 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring',
              active && 'bg-muted font-medium text-foreground'
            )}
          >
            <Icon aria-hidden="true" className="size-4 shrink-0" />
            <span className="min-w-0 truncate" title={label}>
              {label}
            </span>
          </Link>
        ))}
        {visible.length === 0 && (
          <p className="px-2 py-3 text-xs text-muted-foreground">
            No matching pages.
          </p>
        )}
      </nav>
    </>
  )
  return (
    <div className="grid min-w-0 gap-6 lg:grid-cols-[184px_minmax(0,1fr)]">
      <aside
        className="hidden self-start border-r pr-4 lg:sticky lg:top-0 lg:block lg:max-h-[calc(100vh-180px)] lg:overflow-y-auto"
        aria-label={`${title} navigation`}
      >
        {navigation}
      </aside>
      <div className="min-w-0">
        <div className="mb-4 lg:hidden">
          <Sheet open={open} onOpenChange={setOpen}>
            <SheetTrigger asChild>
              <Button variant="outline" size="sm">
                <Menu className="mr-2 size-4" />
                {title} pages
              </Button>
            </SheetTrigger>
            <SheetContent side="left" className="overflow-y-auto">
              <SheetHeader>
                <SheetTitle>{title}</SheetTitle>
              </SheetHeader>
              <div className="mt-4">{navigation}</div>
            </SheetContent>
          </Sheet>
        </div>
        {children}
      </div>
    </div>
  )
}
