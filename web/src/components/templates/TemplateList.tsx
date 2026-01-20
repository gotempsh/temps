import { useState, useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  listTemplatesOptions,
  listTemplateTagsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { TemplateResponse } from '@/api/client/types.gen'
import { TemplateCard } from './TemplateCard'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Search, Star, Loader2, LayoutGrid, List } from 'lucide-react'
import { cn } from '@/lib/utils'

interface TemplateListProps {
  onTemplateSelect: (template: TemplateResponse) => void
  selectedTemplate?: TemplateResponse | null
  showFeaturedFirst?: boolean
}

export function TemplateList({
  onTemplateSelect,
  selectedTemplate,
  showFeaturedFirst = true,
}: TemplateListProps) {
  const [searchQuery, setSearchQuery] = useState('')
  const [selectedTag, setSelectedTag] = useState<string | null>(null)
  const [showFeaturedOnly, setShowFeaturedOnly] = useState(false)
  const [viewMode, setViewMode] = useState<'grid' | 'list'>('grid')

  // Fetch templates
  const { data: templatesData, isLoading: isLoadingTemplates } = useQuery({
    ...listTemplatesOptions({
      query: {
        featured: showFeaturedOnly ? true : undefined,
        tag: selectedTag || undefined,
      },
    }),
  })

  // Fetch tags
  const { data: tagsData } = useQuery({
    ...listTemplateTagsOptions(),
  })

  // Filter and sort templates
  const filteredTemplates = useMemo(() => {
    if (!templatesData?.templates) return []

    let templates = [...templatesData.templates]

    // Filter by search query
    if (searchQuery.trim()) {
      const query = searchQuery.toLowerCase()
      templates = templates.filter(
        (t) =>
          t.name.toLowerCase().includes(query) ||
          t.description?.toLowerCase().includes(query) ||
          t.tags.some((tag) => tag.toLowerCase().includes(query)) ||
          t.preset.toLowerCase().includes(query)
      )
    }

    // Sort: featured first, then alphabetically
    if (showFeaturedFirst) {
      templates.sort((a, b) => {
        if (a.is_featured && !b.is_featured) return -1
        if (!a.is_featured && b.is_featured) return 1
        return a.name.localeCompare(b.name)
      })
    }

    return templates
  }, [templatesData?.templates, searchQuery, showFeaturedFirst])

  if (isLoadingTemplates) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  return (
    <div className="space-y-4">
      {/* Search and filters */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="relative flex-1 max-w-sm">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            placeholder="Search templates..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="pl-9"
          />
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant={showFeaturedOnly ? 'default' : 'outline'}
            size="sm"
            onClick={() => setShowFeaturedOnly(!showFeaturedOnly)}
          >
            <Star className={cn('h-4 w-4 mr-1', showFeaturedOnly && 'fill-current')} />
            Featured
          </Button>
          <div className="flex items-center border rounded-md">
            <Button
              variant={viewMode === 'grid' ? 'secondary' : 'ghost'}
              size="sm"
              className="rounded-r-none"
              onClick={() => setViewMode('grid')}
            >
              <LayoutGrid className="h-4 w-4" />
            </Button>
            <Button
              variant={viewMode === 'list' ? 'secondary' : 'ghost'}
              size="sm"
              className="rounded-l-none"
              onClick={() => setViewMode('list')}
            >
              <List className="h-4 w-4" />
            </Button>
          </div>
        </div>
      </div>

      {/* Tags */}
      {tagsData?.tags && tagsData.tags.length > 0 && (
        <ScrollArea className="w-full whitespace-nowrap">
          <div className="flex gap-2 pb-2">
            <Badge
              variant={selectedTag === null ? 'default' : 'outline'}
              className="cursor-pointer"
              onClick={() => setSelectedTag(null)}
            >
              All
            </Badge>
            {tagsData.tags.map((tag) => (
              <Badge
                key={tag}
                variant={selectedTag === tag ? 'default' : 'outline'}
                className="cursor-pointer"
                onClick={() => setSelectedTag(tag === selectedTag ? null : tag)}
              >
                {tag}
              </Badge>
            ))}
          </div>
        </ScrollArea>
      )}

      {/* Templates grid/list */}
      {filteredTemplates.length === 0 ? (
        <div className="text-center py-12 text-muted-foreground">
          <p>No templates found</p>
          {searchQuery && (
            <p className="text-sm mt-1">
              Try adjusting your search or filters
            </p>
          )}
        </div>
      ) : (
        <div
          className={cn(
            viewMode === 'grid'
              ? 'grid gap-4 sm:grid-cols-2 lg:grid-cols-3'
              : 'flex flex-col gap-3'
          )}
        >
          {filteredTemplates.map((template) => (
            <TemplateCard
              key={template.slug}
              template={template}
              onClick={onTemplateSelect}
              selected={selectedTemplate?.slug === template.slug}
            />
          ))}
        </div>
      )}

      {/* Template count */}
      <div className="text-xs text-muted-foreground text-center pt-2">
        {filteredTemplates.length} of {templatesData?.total ?? 0} templates
      </div>
    </div>
  )
}
