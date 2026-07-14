import { useMemo, useState, useCallback } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  getVisitorsOptions,
  getLiveVisitorsListOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse, VisitorInfo } from '@/api/client/types.gen'
import { Avatar, AvatarFallback } from '@/components/ui/avatar'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Calendar } from '@/components/ui/calendar'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Users,
  ArrowLeft,
  Globe as GlobeIcon,
  ExternalLink,
  Monitor,
  Clock,
  FileText,
  Calendar as CalendarIcon,
} from 'lucide-react'
import { Link, useNavigate } from 'react-router-dom'
import { format, subDays } from 'date-fns'
import type { DateRange } from 'react-day-picker'
import { cn } from '@/lib/utils'
import { EarthGlobe, type ProjectedMarker } from './EarthGlobe'

interface VisitorGlobePageProps {
  project: ProjectResponse
}

// ─── Avatar helpers ────────────────────────────────────────────────

function hashString(str: string): number {
  let hash = 0
  for (let i = 0; i < str.length; i++) {
    const char = str.charCodeAt(i)
    hash = (hash << 5) - hash + char
    hash |= 0
  }
  return Math.abs(hash)
}

const AVATAR_COLORS = [
  { bg: 'bg-red-500/20', text: 'text-red-400', hex: '#f87171' },
  { bg: 'bg-orange-500/20', text: 'text-orange-400', hex: '#fb923c' },
  { bg: 'bg-amber-500/20', text: 'text-amber-400', hex: '#fbbf24' },
  { bg: 'bg-yellow-500/20', text: 'text-yellow-400', hex: '#facc15' },
  { bg: 'bg-lime-500/20', text: 'text-lime-400', hex: '#a3e635' },
  { bg: 'bg-green-500/20', text: 'text-green-400', hex: '#4ade80' },
  { bg: 'bg-emerald-500/20', text: 'text-emerald-400', hex: '#34d399' },
  { bg: 'bg-teal-500/20', text: 'text-teal-400', hex: '#2dd4bf' },
  { bg: 'bg-cyan-500/20', text: 'text-cyan-400', hex: '#22d3ee' },
  { bg: 'bg-sky-500/20', text: 'text-sky-400', hex: '#38bdf8' },
  { bg: 'bg-blue-500/20', text: 'text-blue-400', hex: '#60a5fa' },
  { bg: 'bg-indigo-500/20', text: 'text-indigo-400', hex: '#818cf8' },
  { bg: 'bg-violet-500/20', text: 'text-violet-400', hex: '#a78bfa' },
  { bg: 'bg-purple-500/20', text: 'text-purple-400', hex: '#c084fc' },
  { bg: 'bg-fuchsia-500/20', text: 'text-fuchsia-400', hex: '#e879f9' },
  { bg: 'bg-pink-500/20', text: 'text-pink-400', hex: '#f472b6' },
  { bg: 'bg-rose-500/20', text: 'text-rose-400', hex: '#fb7185' },
]

// Generate a unique two-letter code per visitor from their visitor_id hash
// This ensures every visitor is visually distinct, even from the same city
const INITIALS_CHARSET = 'ABCDEFGHJKLMNPQRSTUVWXYZ' // no I/O to avoid confusion
function uniqueInitials(visitorId: string): string {
  const h = hashString(visitorId)
  const a = INITIALS_CHARSET[h % INITIALS_CHARSET.length]
  const b = INITIALS_CHARSET[Math.floor(h / INITIALS_CHARSET.length) % INITIALS_CHARSET.length]
  return `${a}${b}`
}

function getVisitorAvatar(visitor: VisitorInfo) {
  const hash = hashString(visitor.visitor_id)
  const color = AVATAR_COLORS[hash % AVATAR_COLORS.length]
  const initials = uniqueInitials(visitor.visitor_id)

  return {
    initials,
    colorClass: color.bg,
    textClass: color.text,
    hex: color.hex,
  }
}

function countryCodeToFlag(countryCode: string | null | undefined): string {
  if (!countryCode || countryCode.length !== 2) return ''
  const codePoints = countryCode
    .toUpperCase()
    .split('')
    .map((char) => 127397 + char.charCodeAt(0))
  return String.fromCodePoint(...codePoints)
}

function getTimeAgo(date: Date): string {
  const now = new Date()
  const diffMs = now.getTime() - date.getTime()
  const diffMins = Math.floor(diffMs / 60000)
  if (diffMins < 1) return 'just now'
  if (diffMins < 60) return `${diffMins}m ago`
  const diffHours = Math.floor(diffMins / 60)
  if (diffHours < 24) return `${diffHours}h ago`
  const diffDays = Math.floor(diffHours / 24)
  return `${diffDays}d ago`
}

function getBrowserName(userAgent: string | null | undefined): string | null {
  if (!userAgent) return null
  if (userAgent.includes('Edg')) return 'Edge'
  if (userAgent.includes('Chrome') && !userAgent.includes('Chromium'))
    return 'Chrome'
  if (userAgent.includes('Safari') && !userAgent.includes('Chrome'))
    return 'Safari'
  if (userAgent.includes('Firefox')) return 'Firefox'
  if (userAgent.includes('Opera') || userAgent.includes('OPR')) return 'Opera'
  if (userAgent.includes('bot') || userAgent.includes('Bot')) return 'Bot'
  return null
}

function getOSName(userAgent: string | null | undefined): string | null {
  if (!userAgent) return null
  if (userAgent.includes('Mac OS')) return 'macOS'
  if (userAgent.includes('Windows')) return 'Windows'
  if (userAgent.includes('Linux')) return 'Linux'
  if (userAgent.includes('Android')) return 'Android'
  if (userAgent.includes('iPhone') || userAgent.includes('iPad')) return 'iOS'
  return null
}

// ─── Visitor popover content ─────────────────────────────────────

interface VisitorPopoverProps {
  visitor: VisitorInfo
  projectSlug: string
  isLive: boolean
}

function VisitorPopoverContent({
  visitor,
  projectSlug,
  isLive,
}: VisitorPopoverProps) {
  const flag = countryCodeToFlag(visitor.country_code)
  const location = [visitor.city, visitor.country].filter(Boolean).join(', ')
  const timeAgo = getTimeAgo(new Date(visitor.last_seen))
  const { initials, colorClass, textClass } = getVisitorAvatar(visitor)
  const browser = getBrowserName(visitor.user_agent)
  const os = getOSName(visitor.user_agent)
  const journeyUrl = `/projects/${projectSlug}/analytics/visitors/${visitor.id}`

  return (
    <div className="w-64 space-y-3">
      {/* Header with avatar + location */}
      <div className="flex items-start gap-3">
        <Avatar className="h-10 w-10 flex-shrink-0">
          <AvatarFallback
            className={`${colorClass} ${textClass} text-sm font-semibold`}
          >
            {initials}
          </AvatarFallback>
        </Avatar>
        <div className="min-w-0 flex-1">
          <p className="font-medium text-sm flex items-center gap-1.5">
            {flag && <span>{flag}</span>}
            {location || 'Unknown location'}
            {isLive && (
              <span className="relative flex h-2 w-2 ml-1">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-green-400 opacity-75" />
                <span className="relative inline-flex h-2 w-2 rounded-full bg-green-500" />
              </span>
            )}
          </p>
          <p className="text-xs text-muted-foreground font-mono mt-0.5">
            {visitor.visitor_id.slice(0, 12)}...
          </p>
        </div>
      </div>

      {/* Details */}
      <div className="space-y-1.5 text-xs">
        {visitor.current_page && (
          <div className="flex items-start gap-2 text-muted-foreground">
            <FileText className="h-3.5 w-3.5 flex-shrink-0 mt-0.5" />
            <span className="font-mono truncate">{visitor.current_page}</span>
          </div>
        )}
        {(browser || os) && (
          <div className="flex items-center gap-2 text-muted-foreground">
            <Monitor className="h-3.5 w-3.5 flex-shrink-0" />
            <span>{[browser, os].filter(Boolean).join(' on ')}</span>
          </div>
        )}
        <div className="flex items-center gap-2 text-muted-foreground">
          <Clock className="h-3.5 w-3.5 flex-shrink-0" />
          <span>Last seen {timeAgo}</span>
        </div>
      </div>

      {/* Action — Link instead of navigate to preserve globe state */}
      <Button size="sm" className="w-full" asChild>
        <Link to={journeyUrl}>
          <ExternalLink className="h-3.5 w-3.5 mr-1.5" />
          View Visitor Journey
        </Link>
      </Button>
    </div>
  )
}

// ─── Globe overlay: visitor labels with popover ──────────────────

interface GlobeVisitorOverlaysProps {
  projectedMarkers: ProjectedMarker[]
  projectSlug: string
  liveVisitorIds: Set<string>
}

function GlobeVisitorOverlays({
  projectedMarkers,
  projectSlug,
  liveVisitorIds,
}: GlobeVisitorOverlaysProps) {
  return (
    <div className="absolute inset-0 pointer-events-none overflow-hidden">
      {projectedMarkers.map((pm) => {
        const opacity = Math.min(1, pm.z * 2)
        return (
          <VisitorLabel
            key={pm.visitor.id}
            pm={pm}
            opacity={opacity}
            projectSlug={projectSlug}
            isLive={liveVisitorIds.has(pm.visitor.visitor_id)}
          />
        )
      })}
    </div>
  )
}

interface VisitorLabelProps {
  pm: ProjectedMarker
  opacity: number
  projectSlug: string
  isLive: boolean
}

function VisitorLabel({ pm, opacity, projectSlug, isLive }: VisitorLabelProps) {
  const { initials, hex } = getVisitorAvatar(pm.visitor)
  const flag = countryCodeToFlag(pm.visitor.country_code)
  const city = pm.visitor.city || pm.visitor.country || ''
  // Short unique id suffix from visitor_id for extra differentiation
  const shortId = pm.visitor.visitor_id.slice(-4).toUpperCase()

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          className="absolute pointer-events-auto cursor-pointer transition-opacity duration-150 group"
          style={{
            left: pm.x,
            top: pm.y,
            opacity,
            zIndex: Math.round(pm.z * 100),
            transform: 'translate(-50%, -50%)',
            padding: 0,
            border: 'none',
            background: 'none',
          }}
        >
          {/* Mini card: avatar + visitor identity */}
          <div className="flex items-center gap-1.5 rounded-full pl-0.5 pr-2.5 py-0.5 bg-background/80 backdrop-blur-sm border border-border/50 shadow-lg group-hover:bg-background group-hover:border-border transition-colors">
            {/* Avatar circle — unique color per visitor */}
            <div
              className="w-6 h-6 rounded-full flex items-center justify-center text-[8px] font-bold flex-shrink-0 border border-white/20"
              style={{
                backgroundColor: `${hex}30`,
                color: hex,
              }}
            >
              {initials}
            </div>
            {/* Label: flag + city + short visitor id */}
            <span className="text-[10px] font-medium text-foreground/90 whitespace-nowrap max-w-[100px] truncate leading-none">
              {flag && <span className="mr-0.5">{flag}</span>}
              {city}
              <span className="text-muted-foreground font-mono ml-0.5">#{shortId}</span>
            </span>
            {/* Live indicator */}
            {isLive && (
              <span className="relative flex h-1.5 w-1.5 flex-shrink-0">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-green-400 opacity-75" />
                <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-green-500" />
              </span>
            )}
          </div>
        </button>
      </PopoverTrigger>
      <PopoverContent
        side="top"
        align="center"
        sideOffset={8}
        className="p-3"
        onOpenAutoFocus={(e) => e.preventDefault()}
      >
        <VisitorPopoverContent
          visitor={pm.visitor}
          projectSlug={projectSlug}
          isLive={isLive}
        />
      </PopoverContent>
    </Popover>
  )
}

// ─── Sidebar visitor item ────────────────────────────────────────

interface RecentVisitorItemProps {
  visitor: VisitorInfo
  projectSlug: string
}

function RecentVisitorItem({ visitor, projectSlug }: RecentVisitorItemProps) {
  const flag = countryCodeToFlag(visitor.country_code)
  const location = [visitor.city, visitor.country].filter(Boolean).join(', ')
  const timeAgo = getTimeAgo(new Date(visitor.last_seen))
  const { initials, colorClass, textClass } = getVisitorAvatar(visitor)
  const browser = getBrowserName(visitor.user_agent)
  const os = getOSName(visitor.user_agent)
  const deviceInfo = [browser, os].filter(Boolean).join(' / ')
  const journeyUrl = `/projects/${projectSlug}/analytics/visitors/${visitor.id}`

  return (
    <Link
      to={journeyUrl}
      className="flex items-start gap-3 text-sm p-2 -mx-2 rounded-md cursor-pointer hover:bg-accent/50 transition-colors w-full text-left no-underline"
    >
      <Avatar className="h-8 w-8 flex-shrink-0 mt-0.5">
        <AvatarFallback
          className={`${colorClass} ${textClass} text-xs font-semibold`}
        >
          {initials}
        </AvatarFallback>
      </Avatar>
      <div className="min-w-0 flex-1">
        <p className="truncate font-medium text-xs">
          {flag && <span className="mr-1">{flag}</span>}
          {location || 'Unknown location'}
        </p>
        {visitor.current_page && (
          <p className="truncate text-xs text-muted-foreground font-mono mt-0.5">
            {visitor.current_page}
          </p>
        )}
        <div className="flex items-center gap-2 mt-0.5">
          {deviceInfo && (
            <span className="text-[10px] text-muted-foreground">
              {deviceInfo}
            </span>
          )}
          <span className="text-[10px] text-muted-foreground">{timeAgo}</span>
        </div>
      </div>
      <ExternalLink className="h-3 w-3 text-muted-foreground flex-shrink-0 mt-1" />
    </Link>
  )
}

// ─── Date filter types ───────────────────────────────────────────

const GLOBE_QUICK_FILTERS = [
  { label: 'Today', value: 'today' },
  { label: 'Yesterday', value: 'yesterday' },
  { label: 'Last 7 Days', value: '7days' },
  { label: 'Last 15 Days', value: '15days' },
  { label: 'Last 30 Days', value: '30days' },
  { label: 'Custom', value: 'custom' },
] as const

type GlobeQuickFilter = (typeof GLOBE_QUICK_FILTERS)[number]['value']

interface GlobeDateFilter {
  quickFilter: GlobeQuickFilter
  dateRange: DateRange | undefined
}

function resolveGlobeDateRange(filter: GlobeDateFilter): {
  startDate: Date
  endDate: Date
} {
  const now = new Date()

  if (filter.quickFilter === 'custom' && filter.dateRange?.from) {
    return {
      startDate: filter.dateRange.from,
      endDate: filter.dateRange.to ?? filter.dateRange.from,
    }
  }

  switch (filter.quickFilter) {
    case 'today': {
      const start = new Date(now)
      start.setHours(0, 0, 0, 0)
      return { startDate: start, endDate: now }
    }
    case 'yesterday': {
      const yesterday = new Date(now)
      yesterday.setDate(yesterday.getDate() - 1)
      const start = new Date(yesterday)
      start.setHours(0, 0, 0, 0)
      const end = new Date(yesterday)
      end.setHours(23, 59, 59, 999)
      return { startDate: start, endDate: end }
    }
    case '7days':
      return { startDate: subDays(now, 7), endDate: now }
    case '15days':
      return { startDate: subDays(now, 15), endDate: now }
    case '30days':
      return { startDate: subDays(now, 30), endDate: now }
    default:
      return { startDate: subDays(now, 30), endDate: now }
  }
}

function formatFilterLabel(filter: GlobeDateFilter): string {
  if (filter.quickFilter !== 'custom') {
    return (
      GLOBE_QUICK_FILTERS.find((f) => f.value === filter.quickFilter)?.label ??
      'Last 30 Days'
    )
  }
  if (filter.dateRange?.from) {
    const from = format(filter.dateRange.from, 'MMM d, yyyy')
    const to = filter.dateRange.to
      ? format(filter.dateRange.to, 'MMM d, yyyy')
      : from
    return `${from} - ${to}`
  }
  return 'Custom range'
}

// ─── Main page component ─────────────────────────────────────────

export function VisitorGlobePage({ project }: VisitorGlobePageProps) {
  const navigate = useNavigate()
  const [projectedMarkers, setProjectedMarkers] = useState<ProjectedMarker[]>(
    []
  )
  const [isHovered, setIsHovered] = useState(false)
  const [dateFilter, setDateFilter] = useState<GlobeDateFilter>({
    quickFilter: '30days',
    dateRange: undefined,
  })

  const { startDate, endDate } = useMemo(
    () => resolveGlobeDateRange(dateFilter),
    [dateFilter]
  )

  const { data: visitorsData } = useQuery({
    ...getVisitorsOptions({
      query: {
        project_id: project.id,
        start_date: startDate.toISOString(),
        end_date: endDate.toISOString(),
        limit: 200,
        has_activity_only: true,
      },
    }),
  })

  const { data: liveData } = useQuery({
    ...getLiveVisitorsListOptions({
      query: {
        project_id: project.id,
        window_minutes: 30,
      },
    }),
    refetchInterval: 10000,
  })

  // Build visitor list + live set
  const allVisitors = useMemo(() => {
    const visitors: VisitorInfo[] = []
    const seenIds = new Set<string>()

    if (visitorsData?.visitors) {
      for (const v of visitorsData.visitors) {
        if (v.latitude != null && v.longitude != null) {
          visitors.push(v)
          seenIds.add(v.visitor_id)
        }
      }
    }

    if (liveData?.visitors) {
      for (const lv of liveData.visitors) {
        if (
          lv.latitude != null &&
          lv.longitude != null &&
          !seenIds.has(lv.visitor_id)
        ) {
          visitors.push(lv as unknown as VisitorInfo)
          seenIds.add(lv.visitor_id)
        }
      }
    }

    return visitors
  }, [visitorsData, liveData])

  const liveVisitorIds = useMemo(
    () => new Set(liveData?.visitors?.map((v) => v.visitor_id) ?? []),
    [liveData]
  )

  // Recent visitors for sidebar
  const recentVisitors = useMemo(() => {
    return allVisitors
      .filter((v) => !v.is_crawler)
      .sort(
        (a, b) =>
          new Date(b.last_seen).getTime() - new Date(a.last_seen).getTime()
      )
      .slice(0, 10)
  }, [allVisitors])

  const liveCount = liveData?.visitors?.length ?? 0

  const handleProjectedMarkersUpdate = useCallback(
    (markers: ProjectedMarker[]) => {
      setProjectedMarkers(markers)
    },
    []
  )

  const handleQuickFilter = useCallback((value: GlobeQuickFilter) => {
    setDateFilter({ quickFilter: value, dateRange: undefined })
  }, [])

  const handleCustomDateRange = useCallback((range: DateRange | undefined) => {
    setDateFilter({ quickFilter: 'custom', dateRange: range })
  }, [])

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate(`/projects/${project.slug}/analytics`)}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h2 className="text-xl font-semibold flex items-center gap-2">
              <GlobeIcon className="h-5 w-5" />
              Visitor Globe
            </h2>
            <p className="text-sm text-muted-foreground">
              {visitorsData?.filtered_count ?? 0} visitors &middot;{' '}
              {formatFilterLabel(dateFilter)}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {liveCount > 0 && (
            <Badge
              variant="outline"
              className="gap-1.5 border-green-500/50 text-green-500"
            >
              <span className="relative flex h-2 w-2">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-green-400 opacity-75" />
                <span className="relative inline-flex h-2 w-2 rounded-full bg-green-500" />
              </span>
              {liveCount} live
            </Badge>
          )}
          <Badge variant="secondary" className="gap-1">
            <Users className="h-3 w-3" />
            {allVisitors.length} on globe
          </Badge>
        </div>
      </div>

      {/* Date filters */}
      <div className="flex flex-col sm:flex-row sm:items-center gap-2">
        {/* Quick filter buttons — desktop */}
        <div className="hidden sm:flex gap-1">
          {GLOBE_QUICK_FILTERS.filter((f) => f.value !== 'custom').map(
            (filter) => (
              <Button
                key={filter.value}
                variant={
                  dateFilter.quickFilter === filter.value ? 'default' : 'outline'
                }
                size="sm"
                onClick={() => handleQuickFilter(filter.value)}
              >
                {filter.label}
              </Button>
            )
          )}
        </div>

        {/* Quick filter dropdown — mobile */}
        <div className="sm:hidden">
          <Select
            value={dateFilter.quickFilter}
            onValueChange={(v) => handleQuickFilter(v as GlobeQuickFilter)}
          >
            <SelectTrigger className="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {GLOBE_QUICK_FILTERS.filter((f) => f.value !== 'custom').map(
                (filter) => (
                  <SelectItem key={filter.value} value={filter.value}>
                    {filter.label}
                  </SelectItem>
                )
              )}
            </SelectContent>
          </Select>
        </div>

        {/* Custom date range calendar */}
        <Popover>
          <PopoverTrigger asChild>
            <Button
              variant={
                dateFilter.quickFilter === 'custom' ? 'default' : 'outline'
              }
              size="sm"
              className={cn(
                'min-w-[140px]',
                dateFilter.quickFilter !== 'custom' && 'text-muted-foreground'
              )}
            >
              <CalendarIcon className="mr-2 h-4 w-4" />
              {dateFilter.quickFilter === 'custom' && dateFilter.dateRange?.from
                ? dateFilter.dateRange.to
                  ? `${format(dateFilter.dateRange.from, 'LLL dd, y')} - ${format(dateFilter.dateRange.to, 'LLL dd, y')}`
                  : format(dateFilter.dateRange.from, 'LLL dd, y')
                : 'Custom range'}
            </Button>
          </PopoverTrigger>
          <PopoverContent className="w-auto p-0" align="start">
            <Calendar
              autoFocus
              mode="range"
              defaultMonth={subDays(new Date(), 30)}
              selected={dateFilter.dateRange}
              onSelect={handleCustomDateRange}
              numberOfMonths={2}
              disabled={(date) => date > new Date()}
              endMonth={new Date()}
            />
          </PopoverContent>
        </Popover>
      </div>

      {/* Globe + Sidebar layout */}
      <div className="flex flex-col lg:flex-row gap-6">
        {/* Globe container — hover pauses rotation */}
        <div
          className="flex-1 rounded-lg border bg-card overflow-hidden relative"
          style={{ minHeight: 550 }}
          onMouseEnter={() => setIsHovered(true)}
          onMouseLeave={() => setIsHovered(false)}
        >
          <EarthGlobe
            visitors={allVisitors}
            liveVisitorIds={liveVisitorIds}
            globeSize={550}
            paused={isHovered}
            onProjectedMarkersUpdate={handleProjectedMarkersUpdate}
          />

          {/* Visitor labels with popover on top of the canvas */}
          <GlobeVisitorOverlays
            projectedMarkers={projectedMarkers}
            projectSlug={project.slug}
            liveVisitorIds={liveVisitorIds}
          />
        </div>

        {/* Sidebar — recent visitors */}
        <div className="lg:w-[320px] rounded-lg border bg-card p-5 space-y-4 overflow-hidden">
          <p className="text-xs font-medium text-muted-foreground uppercase tracking-wider">
            Recent visitors
          </p>
          {recentVisitors.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No visitor locations available
            </p>
          ) : (
            <div className="space-y-1">
              {recentVisitors.map((visitor) => (
                <RecentVisitorItem
                  key={visitor.id}
                  visitor={visitor}
                  projectSlug={project.slug}
                />
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
