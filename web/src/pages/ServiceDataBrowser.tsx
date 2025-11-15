import {
  checkExplorerSupportOptions,
  getEntityInfoOptions,
  getServiceOptions,
  listRootContainersOptions,
  queryDataMutation,
} from '@/api/client/@tanstack/react-query.gen'
import {
  downloadObject,
  getEntityInfo,
  listContainersAtPath,
  listEntities,
} from '@/api/client/sdk.gen'
import type {
  ContainerResponse,
  EntityInfoResponse,
  EntityResponse,
  ExplorerSupportResponse,
  FieldResponse,
  QueryDataRequest,
} from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { ScrollArea } from '@/components/ui/scroll-area'
import { ServiceLogo } from '@/components/ui/service-logo'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  ArrowUpDown,
  Box,
  Calendar,
  ChevronDown,
  ChevronRight,
  Download,
  Eye,
  Database,
  File,
  FileText,
  Folder,
  FolderOpen,
  HardDrive,
  Hash,
  Layers,
  Loader2,
  Menu,
  Package,
  RefreshCcw,
  Search,
  SortAsc,
  SortDesc,
  Table as TableIcon,
  Type,
  X,
} from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useNavigate, useParams, useSearchParams } from 'react-router-dom'

interface TreeNode {
  name: string
  path: string
  type: 'container' | 'entity'
  isExpanded?: boolean
  isLoaded?: boolean
  children?: TreeNode[]
  containerType?: string
  entityType?: string
  level?: number // Hierarchy level (0 = root, 1 = first level, etc.)
  canContainContainers?: boolean
  canContainEntities?: boolean
  entityCountHint?: 'small' | 'large' | null // Hint about entity count
}

export function ServiceDataBrowser() {
  const { id } = useParams<{ id: string }>()
  const [searchParams, setSearchParams] = useSearchParams()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  // Parse path and entity from URL - these are the source of truth
  const pathParam = searchParams.get('path') || ''
  const entityParam = searchParams.get('entity') || ''

  // Tree state
  const [treeNodes, setTreeNodes] = useState<TreeNode[]>([])
  const [treeError, setTreeError] = useState<string | null>(null)

  // Sync state with URL params (for component logic that expects state)
  const selectedPath = pathParam
  const selectedEntity = entityParam

  // Track the last expanded path to avoid re-expanding
  const lastExpandedPathRef = useRef<string>('')

  // Filter state (for sidebar tree only)
  const [filterText, setFilterText] = useState('')

  // Sidebar toggle state (mobile responsive) - default closed on mobile, open on desktop
  const [isSidebarOpen, setIsSidebarOpen] = useState(
    typeof window !== 'undefined' ? window.innerWidth >= 768 : true
  )

  // Pagination state
  const [page, setPage] = useState(1)
  const pageSize = 20

  // Data table filter and sort state
  const [dataFilter, setDataFilter] = useState<unknown>(undefined)
  const [dataFilterInput, setDataFilterInput] = useState('') // Local input state before apply
  const [filterFormData, setFilterFormData] = useState<Record<string, any>>({}) // For schema-based filters
  const [dataSortField, setDataSortField] = useState<string>('')
  const [dataSortOrder, setDataSortOrder] = useState<'asc' | 'desc'>('asc')

  // Apply filter handler
  const handleApplyFilter = () => {
    // If we have filter_schema, send the form data as JSON object
    if (explorerSupport?.filter_schema) {
      setDataFilter(filterFormData)
    } else {
      // For SQL capability, send as text (or wrap in object if backend expects it)
      setDataFilter(dataFilterInput || undefined)
    }
    setPage(1) // Reset to first page when filter changes
  }

  // Clear filter handler
  const handleClearFilter = () => {
    setDataFilterInput('')
    setDataFilter(undefined)
    setFilterFormData({})
    setPage(1)
  }

  // Get service details
  const {
    data: service,
    isLoading: serviceLoading,
    error: serviceError,
  } = useQuery({
    ...getServiceOptions({
      path: { id: parseInt(id!) },
    }),
    enabled: !!id,
  })

  // Get explorer support capabilities
  const { data: explorerSupport, isLoading: explorerSupportLoading } = useQuery(
    {
      ...checkExplorerSupportOptions({
        path: { service_id: parseInt(id!) },
      }),
      enabled: !!id,
    }
  )

  // Helper function to get hierarchy capabilities for a given level
  const getHierarchyCapabilities = (level: number) => {
    if (!explorerSupport?.hierarchy || explorerSupport.hierarchy.length === 0) {
      // Fallback: if no hierarchy, assume containers can contain both
      return {
        can_list_containers: true,
        can_list_entities: true,
        container_type: 'folder',
      }
    }

    // Find the hierarchy level configuration
    const hierarchyLevel = explorerSupport.hierarchy.find(
      (h) => h.level === level
    )
    if (!hierarchyLevel) {
      // If level not found, use the last level configuration
      const lastLevel =
        explorerSupport.hierarchy[explorerSupport.hierarchy.length - 1]
      console.warn(`Hierarchy level ${level} not found, using last level:`, lastLevel)
      return {
        can_list_containers: lastLevel.can_list_containers,
        can_list_entities: lastLevel.can_list_entities,
        container_type: lastLevel.container_type,
      }
    }

    console.debug(`Hierarchy capabilities for level ${level}:`, {
      can_list_containers: hierarchyLevel.can_list_containers,
      can_list_entities: hierarchyLevel.can_list_entities,
      container_type: hierarchyLevel.container_type,
    })

    return {
      can_list_containers: hierarchyLevel.can_list_containers,
      can_list_entities: hierarchyLevel.can_list_entities,
      container_type: hierarchyLevel.container_type,
    }
  }

  // Helper function to get appropriate icon for container
  const getContainerIcon = (
    containerType: string | undefined,
    isExpanded: boolean
  ) => {
    const type = containerType?.toLowerCase() || 'folder'
    const className = 'h-4 w-4 text-muted-foreground flex-shrink-0'

    switch (type) {
      case 'bucket':
        // S3 bucket icon
        return <Package className={className} />
      case 'prefix':
        // S3 prefix (folder-like in S3)
        return isExpanded ? (
          <FolderOpen className={className} />
        ) : (
          <Folder className={className} />
        )
      case 'schema':
        return <Database className={className} />
      case 'database':
        return <Database className={className} />
      case 'namespace':
        return <Layers className={className} />
      case 'folder':
      default:
        return isExpanded ? (
          <FolderOpen className={className} />
        ) : (
          <Folder className={className} />
        )
    }
  }

  // Helper function to get appropriate icon for entity
  const getEntityIcon = (entityType: string | undefined) => {
    const type = entityType?.toLowerCase() || 'table'
    const className = 'h-4 w-4 text-muted-foreground flex-shrink-0'

    switch (type) {
      case 'object':
        // S3 object icon
        return <File className={className} />
      case 'table':
        return <TableIcon className={className} />
      case 'view':
        return <FileText className={className} />
      case 'collection':
        return <Box className={className} />
      default:
        return <TableIcon className={className} />
    }
  }

  // Helper function to determine if we're dealing with an object store
  const isObjectStore = () => {
    return explorerSupport?.capabilities?.includes('object-store') || false
  }

  // Helper function to format file size
  const formatFileSize = (bytes: number): string => {
    if (bytes === 0) return '0 Bytes'
    const k = 1024
    const sizes = ['Bytes', 'KB', 'MB', 'GB', 'TB']
    const i = Math.floor(Math.log(bytes) / Math.log(k))
    return Math.round((bytes / Math.pow(k, i)) * 100) / 100 + ' ' + sizes[i]
  }

  // Helper function to format date
  const formatDate = (dateString: string | undefined): string => {
    if (!dateString) return 'N/A'
    try {
      const date = new Date(dateString)
      return date.toLocaleString()
    } catch {
      return dateString
    }
  }

  // Get root containers
  const {
    data: rootContainers,
    isLoading: rootLoading,
    error: rootContainersError,
    refetch: refetchRoot,
  } = useQuery({
    ...listRootContainersOptions({
      path: { service_id: parseInt(id!) },
    }),
    enabled: !!id,
  })

  // Initialize tree with root containers
  useEffect(() => {
    if (rootContainers && treeNodes.length === 0) {
      // Root containers exist at depth 1 (e.g., databases in PostgreSQL)
      // They should get the capabilities from hierarchy level 1 (what databases can do)
      const containerDepth = 1
      const hierarchyInfo = getHierarchyCapabilities(containerDepth)

      const nodes: TreeNode[] = rootContainers.map((container) => ({
        name: container.name,
        path: container.name,
        type: 'container' as const,
        isExpanded: false,
        isLoaded: false,
        children: [],
        level: containerDepth,
        containerType: container.container_type || hierarchyInfo.container_type,
        canContainContainers:
          container.can_contain_containers ?? hierarchyInfo.can_list_containers,
        canContainEntities:
          container.can_contain_entities ?? hierarchyInfo.can_list_entities,
        entityCountHint: (container.entity_count_hint as 'small' | 'large' | null) || null,
      }))
      setTreeNodes(nodes)
    }
  }, [rootContainers, treeNodes.length, explorerSupport])

  // Sync tree expansion with selected path from URL
  useEffect(() => {
    if (!selectedPath || treeNodes.length === 0) return

    // Skip if we've already expanded this exact path
    if (lastExpandedPathRef.current === selectedPath) return

    const pathSegments = selectedPath.split('/')

    // Expand each level of the path sequentially
    const expandPath = async () => {
      for (let i = 0; i < pathSegments.length; i++) {
        const currentPath = pathSegments.slice(0, i + 1).join('/')

        // Find the node at this path
        const findNode = (nodes: TreeNode[], path: string): TreeNode | null => {
          for (const node of nodes) {
            if (node.path === path) return node
            if (node.children) {
              const found = findNode(node.children, path)
              if (found) return found
            }
          }
          return null
        }

        const node = findNode(treeNodes, currentPath)

        // If node exists and can have children
        if (node && node.type === 'container') {
          // If not already expanded and can contain children, expand it
          if (
            !node.isExpanded &&
            (node.canContainContainers || node.canContainEntities)
          ) {
            // Toggle expansion
            setTreeNodes((prevNodes) => {
              const updateNodes = (nodes: TreeNode[]): TreeNode[] => {
                return nodes.map((n) => {
                  if (n.path === currentPath) {
                    return { ...n, isExpanded: true }
                  } else if (n.children) {
                    return { ...n, children: updateNodes(n.children) }
                  }
                  return n
                })
              }
              return updateNodes(prevNodes)
            })

            // Load children if not loaded - wait for it to complete before moving to next level
            if (!node.isLoaded) {
              await loadNodeChildren(currentPath)
              // Wait a bit for state to update
              await new Promise((resolve) => setTimeout(resolve, 100))
            }
          }
        }
      }

      // Mark this path as expanded
      lastExpandedPathRef.current = selectedPath
    }

    expandPath()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedPath])

  // Get entity info when entity is selected
  const { data: entityInfo, isLoading: entityInfoLoading } = useQuery({
    ...getEntityInfoOptions({
      path: {
        service_id: parseInt(id!),
        path: selectedPath,
        entity: selectedEntity,
      },
    }),
    enabled: !!id && !!selectedPath && !!selectedEntity,
  })

  // Query entity data
  const queryEntityData = useMutation({
    ...queryDataMutation(),
  })

  // Load entity data when entity is selected or page changes
  // Skip for S3 objects as they should be downloaded, not queried
  useEffect(() => {
    if (selectedEntity && selectedPath && id) {
      // Check if this is an S3 object (skip query for object stores)
      const isS3Object = entityInfo?.entity_type === 'object' && isObjectStore()

      if (!isS3Object) {
        const queryRequest: QueryDataRequest = {
          limit: pageSize,
          offset: (page - 1) * pageSize,
          sort_by: dataSortField || undefined,
          sort_order: dataSortField ? dataSortOrder : undefined,
          filters: dataFilter || undefined,
        }

        queryEntityData.mutate({
          path: {
            service_id: parseInt(id),
            path: selectedPath,
            entity: selectedEntity,
          },
          body: queryRequest,
        })
      }
    }
    // queryEntityData.mutate is stable and doesn't need to be in dependencies
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    selectedEntity,
    selectedPath,
    page,
    id,
    pageSize,
    dataSortField,
    dataSortOrder,
    dataFilter,
    entityInfo?.entity_type,
  ])

  // Update breadcrumbs
  useEffect(() => {
    const crumbs = [
      { label: 'Storage', href: '/storage' },
      {
        label: service?.service?.name || 'Service',
        href: `/storage/${id}`,
      },
      { label: 'Browse Data', href: `/storage/${id}/browse` },
    ]

    // Break down path into clickable segments
    if (selectedPath) {
      const pathSegments = selectedPath.split('/')
      let accumulatedPath = ''

      pathSegments.forEach((segment, index) => {
        accumulatedPath += (index > 0 ? '/' : '') + segment
        const isLast = index === pathSegments.length - 1 && !selectedEntity

        crumbs.push({
          label: segment,
          href: isLast ? '' : `/storage/${id}/browse?path=${encodeURIComponent(accumulatedPath)}`,
        })
      })
    }

    if (selectedEntity) {
      crumbs.push({ label: selectedEntity, href: '' })
    }

    setBreadcrumbs(crumbs)
  }, [setBreadcrumbs, id, service, selectedPath, selectedEntity])

  usePageTitle(
    `${service?.service?.name || 'Service'} - ${selectedEntity || selectedPath || 'Browse Data'}`
  )

  // Toggle tree node expansion
  const toggleNode = async (nodePath: string) => {
    // Find node BEFORE toggling to check its current state
    const findNode = (nodes: TreeNode[], path: string): TreeNode | null => {
      for (const node of nodes) {
        if (node.path === path) return node
        if (node.children) {
          const found = findNode(node.children, path)
          if (found) return found
        }
      }
      return null
    }

    const node = findNode(treeNodes, nodePath)
    const wasExpanded = node?.isExpanded || false
    const needsLoading = node && !node.isLoaded && !wasExpanded

    // Update tree nodes - toggle expansion
    const updateNodes = (nodes: TreeNode[]): TreeNode[] => {
      return nodes.map((node) => {
        if (node.path === nodePath) {
          // Toggle this node
          return {
            ...node,
            isExpanded: !node.isExpanded,
          }
        } else if (nodePath.startsWith(node.path + '/')) {
          // Recursively update children
          return {
            ...node,
            children: node.children ? updateNodes(node.children) : [],
          }
        }
        return node
      })
    }

    setTreeNodes(updateNodes(treeNodes))

    // Load children if expanding for the first time
    if (needsLoading) {
      await loadNodeChildren(nodePath)
    }
  }

  // Load children for a node
  const loadNodeChildren = async (nodePath: string) => {
    try {
      setTreeError(null) // Clear any previous errors
      let containersData: ContainerResponse[] = []
      let entitiesData: EntityResponse[] = []

      // Find the node to determine what it can contain based on hierarchy
      const findNode = (nodes: TreeNode[], path: string): TreeNode | null => {
        for (const node of nodes) {
          if (node.path === path) return node
          if (node.children) {
            const found = findNode(node.children, path)
            if (found) return found
          }
        }
        return null
      }

      const currentNode = findNode(treeNodes, nodePath)
      const canListContainers = currentNode?.canContainContainers ?? true
      const canListEntities = currentNode?.canContainEntities ?? true

      // Only fetch containers if this node can contain them
      if (canListContainers) {
        try {
          const containersResponse = await listContainersAtPath({
            path: { service_id: parseInt(id!), path: nodePath },
          })
          if (containersResponse.data && Array.isArray(containersResponse.data)) {
            containersData = containersResponse.data
          }
        } catch (error: any) {
          // Only show error if this was supposed to have containers
          if (error?.detail && !error.detail.includes('only supports')) {
            console.error('Error loading containers:', error)
          }
        }
      }

      // Only fetch entities if this node can contain them (and is not a leaf container)
      // For tree loading, we want to show entities that represent sub-containers (like tables in schemas)
      if (canListEntities) {
        try {
          const entitiesResponse = await listEntities({
            path: { service_id: parseInt(id!), path: nodePath },
          })
          // Handle paginated response - extract entities array
          if (entitiesResponse.data) {
            if (Array.isArray(entitiesResponse.data)) {
              // Legacy: Direct array response
              entitiesData = entitiesResponse.data
            } else if (entitiesResponse.data.entities && Array.isArray(entitiesResponse.data.entities)) {
              // New: Paginated response with entities array
              entitiesData = entitiesResponse.data.entities
            }
          }
        } catch (error: any) {
          // Only show error if this was supposed to have entities
          if (error?.detail && !error.detail.includes('requires path depth')) {
            console.error('Error loading entities:', error)
          }
        }
      }

      const updateNodes = (nodes: TreeNode[]): TreeNode[] => {
        return nodes.map((node) => {
          if (node.path === nodePath) {
            // Use entity_count_hint to decide if we should show entities in tree or table
            // "large" means show in paginated table (don't add to tree)
            // "small" or null means we can show in tree
            const shouldShowEntitiesInTable = node.entityCountHint === 'large'

            console.log('Loading children for node:', {
              path: nodePath,
              canContainEntities: node.canContainEntities,
              canContainContainers: node.canContainContainers,
              entityCountHint: node.entityCountHint,
              shouldShowEntitiesInTable,
            })

            if (shouldShowEntitiesInTable) {
              // Mark as loaded but don't add children to tree
              // Children will be displayed in ContainerEntitiesView instead
              console.log('Skipping tree children (large entity count):', nodePath)
              return {
                ...node,
                isLoaded: true,
                children: [],
              }
            }

            const children: TreeNode[] = []
            // Calculate child level (current level + 1)
            const currentLevel = node.level !== undefined ? node.level : 0
            const childLevel = currentLevel + 1
            const childHierarchyInfo = getHierarchyCapabilities(childLevel)

            // Add containers
            containersData.forEach((container: ContainerResponse) => {
              children.push({
                name: container.name,
                path: `${nodePath}/${container.name}`,
                type: 'container',
                isExpanded: false,
                isLoaded: false,
                children: [],
                level: childLevel,
                containerType:
                  container.container_type || childHierarchyInfo.container_type,
                canContainContainers:
                  container.can_contain_containers ??
                  childHierarchyInfo.can_list_containers,
                canContainEntities:
                  container.can_contain_entities ??
                  childHierarchyInfo.can_list_entities,
                entityCountHint:
                  (container.entity_count_hint as 'small' | 'large' | null) || null,
              })
            })

            // Add entities (e.g., PostgreSQL tables, MongoDB collections)
            // These should be added as 'entity' type so clicking them triggers entity data view
            entitiesData.forEach((entity: EntityResponse) => {
              children.push({
                name: entity.name,
                path: `${nodePath}/${entity.name}`,
                type: 'entity', // ← Key change: entities are entities, not containers
                entityType: entity.entity_type,
                level: childLevel,
              })
            })

            return {
              ...node,
              isLoaded: true,
              children,
            }
          } else if (node.children) {
            return {
              ...node,
              children: updateNodes(node.children),
            }
          }
          return node
        })
      }

      setTreeNodes((prevNodes) => updateNodes(prevNodes))
    } catch (error: any) {
      console.error('Failed to load node children:', error)
      setTreeError(error?.detail || 'Failed to load containers and entities')
    }
  }

  // Handle node click
  const handleNodeClick = async (node: TreeNode) => {
    if (node.type === 'container') {
      // Update URL params - use replace to avoid page reload
      setSearchParams({ path: node.path }, { replace: true })
      setPage(1)

      // Find the current node state
      const findNode = (nodes: TreeNode[], path: string): TreeNode | null => {
        for (const n of nodes) {
          if (n.path === path) return n
          if (n.children) {
            const found = findNode(n.children, path)
            if (found) return found
          }
        }
        return null
      }

      const currentNode = findNode(treeNodes, node.path)

      // If this container can only list entities (leaf container like S3 bucket),
      // we don't expand it in the tree - we'll show entities in the main area
      const isLeafContainer =
        node.canContainEntities && !node.canContainContainers

      if (isLeafContainer) {
        // Don't expand in tree, just select it
        // The main content area will show the entities table via ContainerEntitiesView
        // Close sidebar on mobile
        if (window.innerWidth < 768) {
          setIsSidebarOpen(false)
        }
        return
      }

      // For containers that can contain other containers, handle expansion
      if (node.canContainContainers) {
        const isCurrentlyExpanded = currentNode?.isExpanded || false
        const needsLoading =
          currentNode && !currentNode.isLoaded && !isCurrentlyExpanded

        // Toggle expansion state
        const updateNodes = (nodes: TreeNode[]): TreeNode[] => {
          return nodes.map((n) => {
            if (n.path === node.path) {
              return { ...n, isExpanded: !isCurrentlyExpanded }
            } else if (node.path.startsWith(n.path + '/')) {
              return {
                ...n,
                children: n.children ? updateNodes(n.children) : [],
              }
            }
            return n
          })
        }

        setTreeNodes((prevNodes) => updateNodes(prevNodes))

        // Load children if expanding for the first time
        if (needsLoading) {
          await loadNodeChildren(node.path)
        }
      }
    } else if (node.type === 'entity') {
      // Update URL params for entity selection - use replace to avoid page reload
      const parentPath = node.path.split('/').slice(0, -1).join('/')
      setSearchParams(
        {
          path: parentPath,
          entity: node.name,
        },
        { replace: true }
      )
      setPage(1)

      // Close sidebar on mobile when selecting an entity
      if (window.innerWidth < 768) {
        setIsSidebarOpen(false)
      }
    }
  }

  // Filter nodes recursively - shows full tree path to matches
  const filterNodes = (nodes: TreeNode[], searchText: string): TreeNode[] => {
    if (!searchText.trim()) return nodes

    const filtered: TreeNode[] = []
    const lowerSearch = searchText.toLowerCase()

    // Helper function to check if node or any descendant matches
    const hasMatchInTree = (node: TreeNode): boolean => {
      const matchesName = node.name.toLowerCase().includes(lowerSearch)
      const matchesType =
        node.containerType?.toLowerCase().includes(lowerSearch) ||
        node.entityType?.toLowerCase().includes(lowerSearch)

      if (matchesName || matchesType) return true

      if (node.children) {
        return node.children.some((child) => hasMatchInTree(child))
      }

      return false
    }

    for (const node of nodes) {
      // Check if this node or any descendant matches
      if (hasMatchInTree(node)) {
        // Filter children recursively
        const filteredChildren = node.children
          ? filterNodes(node.children, searchText)
          : []

        // Include this node (even if it doesn't match) if it has matching descendants
        // This preserves the full path to matching items
        filtered.push({
          ...node,
          children: filteredChildren,
          // Auto-expand if it has matching children to show the full tree
          isExpanded: filteredChildren.length > 0 ? true : node.isExpanded,
        })
      }
    }

    return filtered
  }

  // Get filtered nodes
  const getProcessedNodes = (): TreeNode[] => {
    if (filterText) {
      return filterNodes(treeNodes, filterText)
    }
    return treeNodes
  }

  // Helper to find selected node
  const findSelectedNode = (
    nodes: TreeNode[],
    path: string
  ): TreeNode | null => {
    for (const node of nodes) {
      if (node.path === path) return node
      if (node.children) {
        const found = findSelectedNode(node.children, path)
        if (found) return found
      }
    }
    return null
  }

  // Helper to render container content
  const renderContainerContent = () => {
    if (!selectedPath) return null

    const selectedNode = findSelectedNode(treeNodes, selectedPath)

    console.log('renderContainerContent:', {
      selectedPath,
      selectedNode,
      canContainEntities: selectedNode?.canContainEntities,
      canContainContainers: selectedNode?.canContainContainers,
      entityCountHint: selectedNode?.entityCountHint,
    })

    // Show entities table if:
    // 1. entity_count_hint is "large" (show in paginated table)
    // 2. OR it's a leaf container (can_contain_entities=true AND can_contain_containers=false)
    const shouldShowEntitiesTable =
      selectedNode &&
      (selectedNode.entityCountHint === 'large' ||
        (selectedNode.canContainEntities === true &&
          selectedNode.canContainContainers === false))

    console.log('shouldShowEntitiesTable check:', {
      shouldShowEntitiesTable,
      entityCountHint: selectedNode?.entityCountHint,
      selectedNode: selectedNode?.name,
      path: selectedPath,
    })

    if (shouldShowEntitiesTable) {
      // Show entities table for leaf containers (like S3 buckets or database tables)
      return (
        <ContainerEntitiesView
          serviceId={id || ''}
          containerPath={selectedPath}
          containerName={selectedPath.split('/').pop() || ''}
          getEntityIcon={getEntityIcon}
          isObjectStore={isObjectStore}
          formatFileSize={formatFileSize}
          formatDate={formatDate}
        />
      )
    }

    // Regular container - show info message
    return (
      <Card>
        <CardHeader>
          <CardTitle>Container: {selectedPath.split('/').pop()}</CardTitle>
          <CardDescription>
            Select an entity from the sidebar to view its data
          </CardDescription>
        </CardHeader>
        <CardContent>
          <p className="text-sm text-muted-foreground">
            Expand folders in the sidebar to navigate through your data
            structure.
          </p>
        </CardContent>
      </Card>
    )
  }

  // Loading state
  if (serviceLoading || rootLoading || explorerSupportLoading) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="p-6">
          <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
        </div>
      </div>
    )
  }

  // Error state - Service load error
  if (serviceError || !service) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="p-6">
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              Failed to load service details. Please try again.
            </AlertDescription>
          </Alert>
        </div>
      </div>
    )
  }

  // Error state - Root containers error
  if (rootContainersError) {
    const error = rootContainersError as any
    const errorTitle = error?.title || 'Connection Error'
    const errorDetail =
      error?.detail ||
      'Failed to connect to the service. Please check the service status and try again.'

    return (
      <div className="flex-1 overflow-hidden flex flex-col">
        {/* Header */}
        <div className="p-6 pb-0">
          <div className="flex items-center gap-3 mb-4">
            <Button
              variant="ghost"
              size="icon"
              onClick={() => navigate(`/storage/${id}`)}
            >
              <ArrowLeft className="h-4 w-4" />
            </Button>
            <ServiceLogo
              service={service.service.service_type}
              className="h-8 w-8"
            />
            <div className="flex flex-col">
              <h1 className="text-2xl font-semibold">
                {service.service.name} - Data Browser
              </h1>
              <p className="text-sm text-muted-foreground">
                Explore containers and browse data
              </p>
            </div>
          </div>
        </div>

        {/* Error state */}
        <div className="flex-1 flex items-center justify-center p-6">
          <Card className="max-w-2xl w-full">
            <CardHeader className="text-center">
              <div className="mx-auto mb-4 h-16 w-16 rounded-full bg-destructive/10 flex items-center justify-center">
                <AlertCircle className="h-8 w-8 text-destructive" />
              </div>
              <CardTitle className="text-xl text-destructive">
                {errorTitle}
              </CardTitle>
              <CardDescription className="text-base mt-2">
                {errorDetail}
              </CardDescription>
            </CardHeader>
            <CardContent className="text-center space-y-3">
              <div className="flex gap-2 justify-center">
                <Button
                  onClick={() => refetchRoot()}
                  variant="default"
                  className="gap-2"
                >
                  <RefreshCcw className="h-4 w-4" />
                  Retry
                </Button>
                <Button
                  onClick={() => navigate(`/storage/${id}`)}
                  variant="outline"
                  className="gap-2"
                >
                  <ArrowLeft className="h-4 w-4" />
                  Back to Service
                </Button>
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    )
  }

  // Check if explorer is supported
  if (explorerSupport && !explorerSupport.supported) {
    return (
      <div className="flex-1 overflow-hidden flex flex-col">
        {/* Header */}
        <div className="p-6 pb-0">
          <div className="flex items-center gap-3 mb-4">
            <Button
              variant="ghost"
              size="icon"
              onClick={() => navigate(`/storage/${id}`)}
            >
              <ArrowLeft className="h-4 w-4" />
            </Button>
            <ServiceLogo
              service={service.service.service_type}
              className="h-8 w-8"
            />
            <div className="flex flex-col">
              <h1 className="text-2xl font-semibold">
                {service.service.name} - Data Browser
              </h1>
              <p className="text-sm text-muted-foreground">
                Explore containers and browse data
              </p>
            </div>
          </div>
        </div>

        {/* Empty state */}
        <div className="flex-1 flex items-center justify-center p-6">
          <Card className="max-w-2xl w-full">
            <CardHeader className="text-center">
              <div className="mx-auto mb-4 h-16 w-16 rounded-full bg-muted flex items-center justify-center">
                <Database className="h-8 w-8 text-muted-foreground" />
              </div>
              <CardTitle className="text-xl">
                Data Explorer Not Available
              </CardTitle>
              <CardDescription className="text-base mt-2">
                The data explorer is not supported for{' '}
                <span className="font-semibold">
                  {explorerSupport.service_type}
                </span>{' '}
                services.
              </CardDescription>
            </CardHeader>
            {explorerSupport.reason && (
              <CardContent className="text-center">
                <Alert>
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>{explorerSupport.reason}</AlertDescription>
                </Alert>
              </CardContent>
            )}
            <CardContent className="text-center pt-0">
              <Button
                onClick={() => navigate(`/storage/${id}`)}
                variant="outline"
                className="gap-2"
              >
                <ArrowLeft className="h-4 w-4" />
                Back to Service
              </Button>
            </CardContent>
          </Card>
        </div>
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-hidden flex flex-col">
      {/* Header */}
      <div className="p-4 md:p-6 pb-0">
        <div className="flex items-center gap-3 mb-4">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate(`/storage/${id}`)}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          {/* Mobile sidebar toggle */}
          <Button
            variant="ghost"
            size="icon"
            className="md:hidden"
            onClick={() => setIsSidebarOpen(!isSidebarOpen)}
          >
            <Menu className="h-4 w-4" />
          </Button>
          <ServiceLogo
            service={service.service.service_type}
            className="h-8 w-8"
          />
          <div className="flex flex-col">
            <h1 className="text-xl md:text-2xl font-semibold">
              {service.service.name} - Data Browser
            </h1>
            <p className="text-xs md:text-sm text-muted-foreground hidden sm:block">
              Explore containers and browse data
            </p>
          </div>
        </div>
      </div>

      {/* Main content area with sidebar */}
      <div className="flex-1 flex gap-0 md:gap-6 px-0 md:px-6 pb-0 md:pb-6 min-h-0 relative overflow-hidden">
        {/* Sidebar - Tree View */}
        <div
          className={`
            ${isSidebarOpen ? 'translate-x-0' : '-translate-x-full'}
            md:translate-x-0
            transition-transform duration-300 ease-in-out
            fixed md:relative
            top-0 left-0 md:left-auto md:top-auto
            z-40
            w-full md:w-80
            h-full
            flex-shrink-0
            px-4 md:px-0
          `}
        >
          <Card className="h-full flex flex-col">
            <CardHeader className="pb-3">
              <CardTitle className="text-base flex items-center gap-2">
                <Database className="h-4 w-4" />
                Containers
              </CardTitle>
              <CardDescription className="text-xs">
                Navigate through your data
              </CardDescription>
            </CardHeader>

            {/* Search Control */}
            <div className="px-4 pb-3">
              <div className="relative">
                <Search className="absolute left-2 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
                <input
                  type="text"
                  placeholder="Filter..."
                  value={filterText}
                  onChange={(e) => setFilterText(e.target.value)}
                  className="w-full pl-8 pr-8 py-1.5 text-sm border rounded-md bg-background focus:outline-none focus:ring-2 focus:ring-ring"
                />
                {filterText && (
                  <button
                    onClick={() => setFilterText('')}
                    className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                  >
                    <X className="h-4 w-4" />
                  </button>
                )}
              </div>
            </div>

            <CardContent className="flex-1 p-0 overflow-hidden border-t min-h-0">
              {/* Show tree error if present */}
              {treeError && (
                <div className="p-4 border-b">
                  <Alert variant="destructive">
                    <AlertCircle className="h-4 w-4" />
                    <AlertDescription className="text-xs">
                      {treeError}
                    </AlertDescription>
                  </Alert>
                  <Button
                    onClick={() => {
                      setTreeError(null)
                      refetchRoot()
                    }}
                    variant="outline"
                    size="sm"
                    className="w-full mt-2 gap-2"
                  >
                    <RefreshCcw className="h-3 w-3" />
                    Retry
                  </Button>
                </div>
              )}
              <ScrollArea className="h-full px-4 pb-4 pt-2">
                {getProcessedNodes().length > 0 ? (
                  <TreeView
                    nodes={getProcessedNodes()}
                    onToggle={toggleNode}
                    onNodeClick={handleNodeClick}
                    selectedPath={selectedPath}
                    selectedEntity={selectedEntity}
                    getContainerIcon={getContainerIcon}
                    getEntityIcon={getEntityIcon}
                  />
                ) : (
                  <div className="text-center py-8 text-sm text-muted-foreground">
                    {filterText ? 'No matches found' : 'No containers'}
                  </div>
                )}
              </ScrollArea>
            </CardContent>
          </Card>
        </div>

        {/* Overlay for mobile when sidebar is open */}
        {isSidebarOpen && (
          <div
            className="fixed inset-0 bg-black/50 z-30 md:hidden"
            onClick={() => setIsSidebarOpen(false)}
          />
        )}

        {/* Main content */}
        <div
          className="flex-1 overflow-y-auto px-4 md:px-0"
          style={{ height: 'calc(100vh - 180px)' }}
        >
          {selectedEntity ? (
            // Show entity data
            <EntityDataView
              entityInfo={entityInfo}
              entityInfoLoading={entityInfoLoading}
              queryResult={queryEntityData.data}
              queryLoading={queryEntityData.isPending}
              queryError={queryEntityData.error}
              page={page}
              pageSize={pageSize}
              onPageChange={setPage}
              dataFilterInput={dataFilterInput}
              onDataFilterInputChange={setDataFilterInput}
              filterFormData={filterFormData}
              onFilterFormDataChange={setFilterFormData}
              appliedFilter={dataFilter}
              onApplyFilter={handleApplyFilter}
              onClearFilter={handleClearFilter}
              dataSortField={dataSortField}
              dataSortOrder={dataSortOrder}
              explorerSupport={explorerSupport}
              onSort={(field: string) => {
                if (dataSortField === field) {
                  // Toggle sort order if same field
                  setDataSortOrder(dataSortOrder === 'asc' ? 'desc' : 'asc')
                } else {
                  // New field, default to ascending
                  setDataSortField(field)
                  setDataSortOrder('asc')
                }
                setPage(1) // Reset to first page when sorting
              }}
              onRefresh={() => {
                if (selectedEntity && selectedPath && id) {
                  queryEntityData.mutate({
                    path: {
                      service_id: parseInt(id),
                      path: selectedPath,
                      entity: selectedEntity,
                    },
                    body: {
                      limit: pageSize,
                      offset: (page - 1) * pageSize,
                      sort_by: dataSortField || undefined,
                      sort_order: dataSortField ? dataSortOrder : undefined,
                      filters: dataFilter || undefined,
                    },
                  })
                }
              }}
              getEntityIcon={getEntityIcon}
              isObjectStore={isObjectStore}
              formatFileSize={formatFileSize}
              formatDate={formatDate}
              serviceId={id || ''}
              containerPath={selectedPath}
              entityName={selectedEntity}
            />
          ) : selectedPath ? (
            renderContainerContent()
          ) : (
            // Show welcome message
            <Card>
              <CardHeader>
                <CardTitle>Welcome to Data Browser</CardTitle>
                <CardDescription>
                  Select a container from the sidebar to get started
                </CardDescription>
              </CardHeader>
              <CardContent>
                <p className="text-sm text-muted-foreground">
                  Use the tree navigation on the left to browse through
                  containers, schemas, and tables.
                </p>
              </CardContent>
            </Card>
          )}
        </div>
      </div>
    </div>
  )
}

// Tree View Component
function TreeView({
  nodes,
  level = 0,
  onToggle,
  onNodeClick,
  selectedPath,
  selectedEntity,
  getContainerIcon,
  getEntityIcon,
}: {
  nodes: TreeNode[]
  level?: number
  onToggle: (path: string) => void
  onNodeClick: (node: TreeNode) => void
  selectedPath: string
  selectedEntity: string
  getContainerIcon: (
    containerType: string | undefined,
    isExpanded: boolean
  ) => React.ReactElement
  getEntityIcon: (entityType: string | undefined) => React.ReactElement
}) {
  return (
    <div className="space-y-1">
      {nodes.map((node) => (
        <TreeNodeComponent
          key={node.path}
          node={node}
          level={level}
          onToggle={onToggle}
          onNodeClick={onNodeClick}
          selectedPath={selectedPath}
          selectedEntity={selectedEntity}
          getContainerIcon={getContainerIcon}
          getEntityIcon={getEntityIcon}
        />
      ))}
    </div>
  )
}

// Tree Node Component
function TreeNodeComponent({
  node,
  level,
  onToggle,
  onNodeClick,
  selectedPath,
  selectedEntity,
  getContainerIcon,
  getEntityIcon,
}: {
  node: TreeNode
  level: number
  onToggle: (path: string) => void
  onNodeClick: (node: TreeNode) => void
  selectedPath: string
  selectedEntity: string
  getContainerIcon: (
    containerType: string | undefined,
    isExpanded: boolean
  ) => React.ReactElement
  getEntityIcon: (entityType: string | undefined) => React.ReactElement
}) {
  const isSelected =
    node.type === 'container'
      ? node.path === selectedPath && !selectedEntity
      : node.path === `${selectedPath}/${selectedEntity}`

  // Only show chevron if:
  // 1. It's a container
  // 2. It can contain containers
  // 3. AND entity_count_hint is NOT "large" (large means show entities in table, not tree)
  const canExpand =
    node.type === 'container' &&
    node.canContainContainers &&
    node.entityCountHint !== 'large'

  return (
    <div>
      <button
        onClick={() => {
          // Only call onNodeClick - it handles the toggle internally
          onNodeClick(node)
        }}
        className={`w-full flex items-center gap-2 px-2 py-1.5 text-sm rounded-md transition-colors hover:bg-accent ${
          isSelected ? 'bg-accent text-accent-foreground' : ''
        }`}
        style={{ paddingLeft: `${level * 16 + 8}px` }}
      >
        {canExpand && (
          <span className="flex-shrink-0">
            {node.isExpanded ? (
              <ChevronDown className="h-3.5 w-3.5" />
            ) : (
              <ChevronRight className="h-3.5 w-3.5" />
            )}
          </span>
        )}
        {node.type === 'container'
          ? getContainerIcon(node.containerType, node.isExpanded || false)
          : getEntityIcon(node.entityType)}
        <span className="truncate flex-1 text-left">{node.name}</span>
        {node.containerType && (
          <Badge variant="outline" className="text-xs flex-shrink-0">
            {node.containerType}
          </Badge>
        )}
      </button>
      {node.isExpanded && node.children && node.children.length > 0 && (
        <TreeView
          nodes={node.children}
          level={level + 1}
          onToggle={onToggle}
          onNodeClick={onNodeClick}
          selectedPath={selectedPath}
          selectedEntity={selectedEntity}
          getContainerIcon={getContainerIcon}
          getEntityIcon={getEntityIcon}
        />
      )}
    </div>
  )
}

// Dynamic Filter Builder Component
function DynamicFilterBuilder({
  schema,
  formData,
  onFormDataChange,
  onApplyFilter,
}: {
  schema: any
  formData: Record<string, any>
  onFormDataChange: (data: Record<string, any>) => void
  onApplyFilter?: () => void
}) {
  if (!schema || !schema.properties) {
    return null
  }

  const handleFieldChange = (fieldName: string, value: any) => {
    onFormDataChange({
      ...formData,
      [fieldName]: value,
    })
  }

  const renderField = (fieldName: string, fieldSchema: any) => {
    const value = formData[fieldName] || ''
    const type = fieldSchema.type
    const title = fieldSchema.title || fieldName
    const description = fieldSchema.description
    const uiWidget = fieldSchema['x-ui-widget'] // UI widget type
    const uiPlaceholder = fieldSchema['x-ui-placeholder'] // Custom placeholder
    const uiRows = fieldSchema['x-ui-rows'] || 3 // Textarea rows
    const examples = fieldSchema.examples || []

    // Enum/Select field
    if (fieldSchema.enum) {
      return (
        <div key={fieldName} className="space-y-2">
          <Label htmlFor={fieldName}>{title}</Label>
          {description && (
            <p className="text-xs text-muted-foreground">{description}</p>
          )}
          <Select
            value={value}
            onValueChange={(val) => handleFieldChange(fieldName, val)}
          >
            <SelectTrigger>
              <SelectValue
                placeholder={uiPlaceholder || `Select ${title.toLowerCase()}`}
              />
            </SelectTrigger>
            <SelectContent>
              {fieldSchema.enum.map((option: any) => (
                <SelectItem key={option} value={String(option)}>
                  {String(option)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      )
    }

    // Textarea widget or long text
    if (uiWidget === 'textarea' || fieldSchema.maxLength > 200) {
      return (
        <div key={fieldName} className="space-y-2">
          <Label htmlFor={fieldName}>{title}</Label>
          {description && (
            <p className="text-xs text-muted-foreground">{description}</p>
          )}
          {examples.length > 0 && (
            <details className="text-xs text-muted-foreground">
              <summary className="cursor-pointer hover:text-foreground">
                Show examples
              </summary>
              <ul className="mt-1 ml-4 list-disc space-y-1">
                {examples.map((ex: string, i: number) => (
                  <li key={i} className="font-mono">
                    {ex}
                  </li>
                ))}
              </ul>
            </details>
          )}
          <Textarea
            id={fieldName}
            value={value}
            onChange={(e) => handleFieldChange(fieldName, e.target.value)}
            onKeyDown={(e) => {
              // Apply filter on Ctrl+Enter or Cmd+Enter
              if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
                e.preventDefault()
                if (onApplyFilter) {
                  onApplyFilter()
                }
              }
            }}
            placeholder={uiPlaceholder || `Enter ${title.toLowerCase()}`}
            rows={uiRows}
            className="font-mono text-sm"
          />
        </div>
      )
    }

    // Number input
    if (type === 'number' || type === 'integer') {
      return (
        <div key={fieldName} className="space-y-2">
          <Label htmlFor={fieldName}>{title}</Label>
          {description && (
            <p className="text-xs text-muted-foreground">{description}</p>
          )}
          <Input
            id={fieldName}
            type="number"
            value={value}
            onChange={(e) =>
              handleFieldChange(
                fieldName,
                type === 'integer'
                  ? parseInt(e.target.value) || 0
                  : parseFloat(e.target.value) || 0
              )
            }
            placeholder={uiPlaceholder || `Enter ${title.toLowerCase()}`}
            min={fieldSchema.minimum}
            max={fieldSchema.maximum}
          />
        </div>
      )
    }

    // Boolean/checkbox
    if (type === 'boolean') {
      return (
        <div key={fieldName} className="flex items-center space-x-2">
          <input
            id={fieldName}
            type="checkbox"
            checked={value || false}
            onChange={(e) => handleFieldChange(fieldName, e.target.checked)}
            className="h-4 w-4 rounded border-input"
          />
          <Label htmlFor={fieldName} className="font-normal">
            {title}
            {description && (
              <span className="text-xs text-muted-foreground ml-2">
                ({description})
              </span>
            )}
          </Label>
        </div>
      )
    }

    // Default: String input
    return (
      <div key={fieldName} className="space-y-2">
        <Label htmlFor={fieldName}>{title}</Label>
        {description && (
          <p className="text-xs text-muted-foreground">{description}</p>
        )}
        <Input
          id={fieldName}
          type="text"
          value={value}
          onChange={(e) => handleFieldChange(fieldName, e.target.value)}
          placeholder={uiPlaceholder || `Enter ${title.toLowerCase()}`}
          maxLength={fieldSchema.maxLength}
        />
      </div>
    )
  }

  return (
    <div className="space-y-4">
      {Object.entries(schema.properties).map(
        ([fieldName, fieldSchema]: [string, any]) =>
          renderField(fieldName, fieldSchema)
      )}
    </div>
  )
}

// Container Entities View Component - Shows entities in a leaf container (like S3 bucket)
function ContainerEntitiesView({
  serviceId,
  containerPath,
  containerName,
  getEntityIcon,
  isObjectStore,
  formatFileSize,
  formatDate,
}: {
  serviceId: string
  containerPath: string
  containerName: string
  getEntityIcon: (entityType: string | undefined) => React.ReactElement
  isObjectStore: () => boolean
  formatFileSize: (bytes: number) => string
  formatDate: (dateString: string | undefined) => string
}) {
  const [nextToken, setNextToken] = useState<string | null>(null)
  const [selectedEntityForInfo, setSelectedEntityForInfo] = useState<string | null>(
    null
  )
  const pageSize = 20

  // Fetch entities at this container path
  const {
    data: entitiesResponse,
    isLoading,
    error,
    refetch,
  } = useQuery({
    queryKey: ['container-entities', serviceId, containerPath, nextToken],
    queryFn: async () => {
      const response = await listEntities({
        path: { service_id: parseInt(serviceId), path: containerPath },
        query: {
          limit: pageSize,
          token: nextToken || undefined,
        },
      })
      return response.data
    },
    enabled: !!serviceId && !!containerPath,
  })

  // Fetch entity info when an entity is selected
  const {
    data: entityInfo,
    isLoading: entityInfoLoading,
    error: entityInfoError,
  } = useQuery({
    queryKey: ['entity-info', serviceId, containerPath, selectedEntityForInfo],
    queryFn: async () => {
      if (!selectedEntityForInfo) return null
      const response = await getEntityInfo({
        path: {
          service_id: parseInt(serviceId),
          path: containerPath,
          entity: selectedEntityForInfo,
        },
      })
      return response.data
    },
    enabled: !!serviceId && !!containerPath && !!selectedEntityForInfo,
  })

  if (isLoading) {
    return (
      <Card>
        <CardHeader>
          <div className="flex items-center justify-between">
            <div className="space-y-2 flex-1">
              <Skeleton className="h-6 w-48" />
              <Skeleton className="h-4 w-64" />
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <div className="space-y-3">
            <Skeleton className="h-12 w-full" />
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
          </div>
        </CardContent>
      </Card>
    )
  }

  if (error) {
    const err = error as any
    // Extract detailed error information
    const errorDetail = err?.detail || err?.message || err?.error?.detail || 'Failed to load entities'
    const errorTitle = err?.title || err?.error?.title || 'Error'

    console.error('ContainerEntitiesView error:', {
      containerPath,
      error: err,
      detail: errorDetail,
      title: errorTitle,
    })

    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Database className="h-5 w-5" />
            {containerName}
          </CardTitle>
          <CardDescription className="text-xs text-muted-foreground mt-1">
            Path: {containerPath}
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              <div className="space-y-2">
                <div className="font-medium">{errorTitle}</div>
                <div className="text-sm">{errorDetail}</div>
                {err?.status && (
                  <div className="text-xs opacity-70">Status: {err.status}</div>
                )}
              </div>
            </AlertDescription>
          </Alert>
        </CardContent>
      </Card>
    )
  }

  const entitiesList = entitiesResponse?.entities || []
  const hasMore = entitiesResponse?.has_more || false
  const total = entitiesResponse?.total
  const count = entitiesResponse?.count || 0

  // Split path into segments for display
  const pathSegments = containerPath.split('/')

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div className="flex-1">
            <div className="flex items-center gap-2 text-sm text-muted-foreground mb-2">
              {pathSegments.map((segment, index) => (
                <div key={index} className="flex items-center gap-2">
                  {index > 0 && <span>/</span>}
                  <span className={index === pathSegments.length - 1 ? 'font-medium text-foreground' : ''}>
                    {segment}
                  </span>
                </div>
              ))}
            </div>
            <CardTitle className="flex items-center gap-2">
              <Database className="h-5 w-5" />
              {containerName}
            </CardTitle>
            <CardDescription>
              Showing {count} {isObjectStore() ? 'objects' : 'entities'}
              {total !== null && total !== undefined && ` of ${total}`}
            </CardDescription>
          </div>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              setNextToken(null)
              refetch()
            }}
            className="gap-2"
          >
            <RefreshCcw className="h-4 w-4" />
            Refresh
          </Button>
        </div>
      </CardHeader>
      <CardContent>
        {entitiesList.length > 0 ? (
          <>
            <div className="rounded-md border overflow-x-auto">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b bg-muted/50">
                    <th className="text-left p-3 font-medium whitespace-nowrap">
                      Type
                    </th>
                    <th className="text-left p-3 font-medium whitespace-nowrap">
                      Name
                    </th>
                    {isObjectStore() && (
                      <>
                        <th className="text-left p-3 font-medium whitespace-nowrap">
                          Content Type
                        </th>
                        <th className="text-left p-3 font-medium whitespace-nowrap">
                          Size
                        </th>
                        <th className="text-left p-3 font-medium whitespace-nowrap">
                          Last Modified
                        </th>
                        <th className="text-right p-3 font-medium whitespace-nowrap">
                          Actions
                        </th>
                      </>
                    )}
                  </tr>
                </thead>
                <tbody>
                  {entitiesList.map((entity: EntityResponse, idx: number) => (
                    <tr
                      key={`${entity.name}-${idx}`}
                      className="border-b last:border-0 hover:bg-muted/30"
                    >
                      <td className="p-3">
                        <div className="[&>svg]:h-4 [&>svg]:w-4">
                          {getEntityIcon(entity.entity_type)}
                        </div>
                      </td>
                      <td className="p-3 font-mono text-xs">{entity.name}</td>
                      {isObjectStore() && (
                        <>
                          <td className="p-3 text-xs">
                            {(entity as any).metadata?.content_type ||
                              (entity as any).content_type ||
                              '-'}
                          </td>
                          <td className="p-3 text-xs">
                            {(entity as any).size_bytes !== undefined
                              ? formatFileSize((entity as any).size_bytes)
                              : '-'}
                          </td>
                          <td className="p-3 text-xs">
                            {(entity as any).last_modified
                              ? formatDate((entity as any).last_modified)
                              : '-'}
                          </td>
                          <td className="p-3 text-right">
                            <div className="flex items-center justify-end gap-1">
                              <Button
                                variant="ghost"
                                size="sm"
                                className="h-8 px-2"
                                onClick={async () => {
                                  try {
                                    const response = await downloadObject({
                                      path: {
                                        service_id: parseInt(serviceId),
                                        path: containerPath,
                                        entity: entity.name,
                                      },
                                    })

                                    // Ensure we have a Blob
                                    let blob: Blob
                                    const data = response.data as any
                                    if (data instanceof Blob) {
                                      blob = data
                                    } else if (typeof data === 'string') {
                                      // Convert string to Blob
                                      blob = new Blob([data], {
                                        type: 'application/octet-stream',
                                      })
                                    } else if (data) {
                                      // Convert other data types to JSON string then Blob
                                      const jsonStr = JSON.stringify(data)
                                      blob = new Blob([jsonStr], {
                                        type: 'application/json',
                                      })
                                    } else {
                                      throw new Error('No data received from server')
                                    }

                                    const url = window.URL.createObjectURL(blob)
                                    const a = document.createElement('a')
                                    a.href = url
                                    a.download = entity.name
                                    document.body.appendChild(a)
                                    a.click()
                                    window.URL.revokeObjectURL(url)
                                    document.body.removeChild(a)
                                  } catch (error) {
                                    console.error('Failed to download object:', error)
                                  }
                                }}
                                title="Download"
                              >
                                <Download className="h-4 w-4" />
                              </Button>
                              <Button
                                variant="ghost"
                                size="sm"
                                className="h-8 px-2"
                                onClick={() => {
                                  setSelectedEntityForInfo(entity.name)
                                }}
                                title="View Info"
                              >
                                <Eye className="h-4 w-4" />
                              </Button>
                            </div>
                          </td>
                        </>
                      )}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>

            {/* Pagination Controls */}
            <div className="flex items-center justify-between mt-4">
              <div className="text-sm text-muted-foreground">
                {count} {isObjectStore() ? 'objects' : 'entities'} shown
                {total !== null && total !== undefined && ` of ${total} total`}
              </div>
              <div className="flex items-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={!nextToken}
                  onClick={() => setNextToken(null)}
                >
                  First Page
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={!hasMore}
                  onClick={() =>
                    setNextToken(entitiesResponse?.next_token || null)
                  }
                >
                  Next Page
                </Button>
              </div>
            </div>

            {/* Entity Info Modal */}
            <Dialog
              open={!!selectedEntityForInfo}
              onOpenChange={(open) => {
                if (!open) setSelectedEntityForInfo(null)
              }}
            >
              <DialogContent className="max-w-4xl max-h-[80vh] overflow-y-auto">
                <DialogHeader>
                  <DialogTitle className="flex items-center gap-2">
                    <FileText className="h-5 w-5" />
                    Entity Info: {selectedEntityForInfo}
                  </DialogTitle>
                </DialogHeader>

                {entityInfoLoading && (
                  <div className="flex items-center justify-center py-8">
                    <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
                  </div>
                )}

                {entityInfoError && (
                  <Alert variant="destructive">
                    <AlertCircle className="h-4 w-4" />
                    <AlertDescription>
                      Failed to load entity info
                    </AlertDescription>
                  </Alert>
                )}

                {entityInfo && !entityInfoLoading && (
                  <div className="space-y-6">
                    {/* Metadata Section */}
                    {entityInfo.metadata &&
                    typeof entityInfo.metadata === 'object' &&
                    entityInfo.metadata !== null ? (
                      <div>
                        <h4 className="font-semibold mb-3">Metadata</h4>
                        <div className="rounded-md border">
                          <table className="w-full text-sm">
                            <tbody>
                              {Object.entries(
                                entityInfo.metadata as Record<string, any>
                              ).map(([key, value]) => (
                                <tr key={key} className="border-b last:border-0">
                                  <td className="p-3 font-medium bg-muted/50 w-1/3">
                                    {key}
                                  </td>
                                  <td className="p-3 font-mono text-xs break-all">
                                    {value === null
                                      ? 'null'
                                      : typeof value === 'object'
                                        ? JSON.stringify(value, null, 2)
                                        : String(value)}
                                  </td>
                                </tr>
                              ))}
                            </tbody>
                          </table>
                        </div>
                      </div>
                    ) : null}

                    {/* Fields Section */}
                    {entityInfo.fields && entityInfo.fields.length > 0 && (
                      <div>
                        <h4 className="font-semibold mb-3">Fields</h4>
                        <div className="rounded-md border">
                          <table className="w-full text-sm">
                            <thead>
                              <tr className="border-b bg-muted/50">
                                <th className="text-left p-3 font-medium">Name</th>
                                <th className="text-left p-3 font-medium">Type</th>
                                <th className="text-left p-3 font-medium">
                                  Nullable
                                </th>
                              </tr>
                            </thead>
                            <tbody>
                              {entityInfo.fields.map((field) => (
                                <tr key={field.name} className="border-b last:border-0">
                                  <td className="p-3 font-mono text-xs">
                                    {field.name}
                                  </td>
                                  <td className="p-3 text-xs">{field.field_type}</td>
                                  <td className="p-3 text-xs">
                                    {field.nullable ? 'Yes' : 'No'}
                                  </td>
                                </tr>
                              ))}
                            </tbody>
                          </table>
                        </div>
                      </div>
                    )}

                    {/* Additional Info */}
                    {(entityInfo.size_bytes !== null &&
                      entityInfo.size_bytes !== undefined) ||
                    (entityInfo.row_count !== null &&
                      entityInfo.row_count !== undefined) ? (
                      <div className="grid grid-cols-2 gap-4">
                        {entityInfo.size_bytes !== null &&
                          entityInfo.size_bytes !== undefined && (
                            <div>
                              <div className="text-sm font-medium text-muted-foreground">
                                Size
                              </div>
                              <div className="text-lg font-semibold">
                                {formatFileSize(entityInfo.size_bytes)}
                              </div>
                            </div>
                          )}
                        {entityInfo.row_count !== null &&
                          entityInfo.row_count !== undefined && (
                            <div>
                              <div className="text-sm font-medium text-muted-foreground">
                                Row Count
                              </div>
                              <div className="text-lg font-semibold">
                                {entityInfo.row_count.toLocaleString()}
                              </div>
                            </div>
                          )}
                      </div>
                    ) : null}
                  </div>
                )}
              </DialogContent>
            </Dialog>
          </>
        ) : (
          <div className="text-center py-8 text-sm text-muted-foreground">
            No {isObjectStore() ? 'objects' : 'entities'} found in this
            container
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// Entity Data View Component
function EntityDataView({
  entityInfo,
  entityInfoLoading,
  queryResult,
  queryLoading,
  queryError,
  page,
  pageSize,
  onPageChange,
  dataFilterInput,
  onDataFilterInputChange,
  filterFormData,
  onFilterFormDataChange,
  appliedFilter,
  onApplyFilter,
  onClearFilter,
  dataSortField,
  dataSortOrder,
  explorerSupport,
  onSort,
  onRefresh,
  getEntityIcon,
  isObjectStore,
  formatFileSize,
  formatDate,
  serviceId,
  containerPath,
  entityName,
}: {
  entityInfo?: EntityInfoResponse
  entityInfoLoading: boolean
  queryResult?: any
  queryLoading: boolean
  queryError: any
  page: number
  pageSize: number
  onPageChange: (page: number) => void
  dataFilterInput: string
  onDataFilterInputChange: (filter: string) => void
  filterFormData: Record<string, any>
  onFilterFormDataChange: (data: Record<string, any>) => void
  appliedFilter: unknown
  onApplyFilter: () => void
  onClearFilter: () => void
  dataSortField: string
  dataSortOrder: 'asc' | 'desc'
  explorerSupport?: ExplorerSupportResponse
  onSort: (field: string) => void
  onRefresh: () => void
  getEntityIcon: (entityType: string | undefined) => React.ReactElement
  isObjectStore: () => boolean
  formatFileSize: (bytes: number) => string
  formatDate: (dateString: string | undefined) => string
  serviceId: string
  containerPath: string
  entityName: string
}) {
  const [showSchema, setShowSchema] = useState(false)
  const [isFilterExpanded, setIsFilterExpanded] = useState(false)
  const [isDownloading, setIsDownloading] = useState(false)

  // Handle streaming download for S3 objects
  const handleDownload = async () => {
    if (!serviceId || !containerPath || !entityName) return

    try {
      setIsDownloading(true)

      // Construct the download URL using the correct endpoint
      const downloadUrl = `/api/external-services/${serviceId}/query/containers/${containerPath}/entities/${entityName}/download`

      // Fetch the file as a stream
      const response = await fetch(downloadUrl)

      if (!response.ok) {
        throw new Error(`Download failed: ${response.statusText}`)
      }

      // Get the blob from the response
      const blob = await response.blob()

      // Create a download link
      const url = window.URL.createObjectURL(blob)
      const link = document.createElement('a')
      link.href = url
      link.download = entityName
      document.body.appendChild(link)
      link.click()
      document.body.removeChild(link)
      window.URL.revokeObjectURL(url)
    } catch (error) {
      console.error('Download failed:', error)
      // You might want to show a toast notification here
    } finally {
      setIsDownloading(false)
    }
  }

  // Check if SQL capability is available (for filter support)
  const hasSqlCapability =
    explorerSupport?.capabilities.includes('sql') || false
  const hasFilterSchema = explorerSupport?.filter_schema !== undefined
  const hasFilterSupport = hasFilterSchema || hasSqlCapability

  // Show skeleton loading while data is being fetched
  if (entityInfoLoading || queryLoading) {
    return (
      <div className="space-y-6">
        {/* Entity Info Card Skeleton */}
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div className="space-y-2 flex-1">
                <Skeleton className="h-6 w-48" />
                <Skeleton className="h-4 w-64" />
              </div>
              <div className="flex items-center gap-2">
                <Skeleton className="h-9 w-32" />
                <Skeleton className="h-9 w-24" />
              </div>
            </div>
          </CardHeader>
        </Card>

        {/* Data Table Card Skeleton */}
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div className="space-y-2">
                <Skeleton className="h-6 w-32" />
                <Skeleton className="h-4 w-96" />
              </div>
            </div>
            {/* Filter skeleton */}
            <div className="mt-4 space-y-3">
              <Skeleton className="h-10 w-full" />
              <div className="flex gap-2">
                <Skeleton className="h-10 w-32" />
                <Skeleton className="h-10 w-24" />
              </div>
            </div>
          </CardHeader>
          <CardContent>
            {/* Table skeleton */}
            <div className="space-y-3">
              <Skeleton className="h-12 w-full" />
              <Skeleton className="h-10 w-full" />
              <Skeleton className="h-10 w-full" />
              <Skeleton className="h-10 w-full" />
              <Skeleton className="h-10 w-full" />
              <Skeleton className="h-10 w-full" />
            </div>
            {/* Pagination skeleton */}
            <div className="flex items-center justify-between mt-4">
              <Skeleton className="h-4 w-48" />
              <div className="flex items-center gap-2">
                <Skeleton className="h-9 w-24" />
                <Skeleton className="h-9 w-24" />
              </div>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  // Extract error if present (but don't block rendering)
  const error = queryError as any
  const errorTitle = error?.title
  const errorDetail = error?.detail

  return (
    <div className="space-y-6">
      {/* Entity Info Card */}
      {entityInfo && (
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div>
                <CardTitle className="flex items-center gap-2">
                  <div className="[&>svg]:h-5 [&>svg]:w-5">
                    {getEntityIcon(entityInfo.entity_type)}
                  </div>
                  {entityInfo.entity}
                </CardTitle>
                <CardDescription>
                  Type: {entityInfo.entity_type}
                  {!isObjectStore() && entityInfo.fields && (
                    <> • {entityInfo.fields.length} fields</>
                  )}
                </CardDescription>
              </div>
              <div className="flex items-center gap-2">
                {/* Download button for S3 objects */}
                {isObjectStore() && entityInfo.entity_type === 'object' && (
                  <Button
                    variant="default"
                    size="sm"
                    onClick={handleDownload}
                    disabled={isDownloading}
                    className="gap-2"
                  >
                    {isDownloading ? (
                      <>
                        <Loader2 className="h-4 w-4 animate-spin" />
                        Downloading...
                      </>
                    ) : (
                      <>
                        <Download className="h-4 w-4" />
                        Download
                      </>
                    )}
                  </Button>
                )}
                {!isObjectStore() && entityInfo.fields && (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setShowSchema(!showSchema)}
                  >
                    {showSchema ? 'Hide' : 'Show'} Schema
                  </Button>
                )}
                {/* Only show Refresh button for non-S3-objects */}
                {!(isObjectStore() && entityInfo.entity_type === 'object') && (
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={onRefresh}
                    className="gap-2"
                  >
                    <RefreshCcw className="h-4 w-4" />
                    Refresh
                  </Button>
                )}
              </div>
            </div>
          </CardHeader>

          {/* Show object metadata for S3 objects */}
          {isObjectStore() &&
            entityInfo.entity_type === 'object' &&
            (entityInfo as any).metadata && (
              <CardContent className="pt-0">
                <div className="grid grid-cols-1 md:grid-cols-2 gap-4 pt-4">
                  {/* File Size */}
                  {(entityInfo as any).size_bytes !== undefined && (
                    <div className="flex items-start gap-3">
                      <div className="p-2 rounded-md bg-muted">
                        <HardDrive className="h-4 w-4 text-muted-foreground" />
                      </div>
                      <div className="flex-1 min-w-0">
                        <p className="text-sm font-medium text-muted-foreground">
                          Size
                        </p>
                        <p className="text-base font-mono break-all">
                          {formatFileSize((entityInfo as any).size_bytes)}
                        </p>
                      </div>
                    </div>
                  )}

                  {/* Content Type */}
                  {(entityInfo as any).metadata.content_type && (
                    <div className="flex items-start gap-3">
                      <div className="p-2 rounded-md bg-muted">
                        <Type className="h-4 w-4 text-muted-foreground" />
                      </div>
                      <div className="flex-1 min-w-0">
                        <p className="text-sm font-medium text-muted-foreground">
                          Content Type
                        </p>
                        <p className="text-base font-mono break-all">
                          {(entityInfo as any).metadata.content_type}
                        </p>
                      </div>
                    </div>
                  )}

                  {/* Last Modified */}
                  {(entityInfo as any).metadata.last_modified && (
                    <div className="flex items-start gap-3">
                      <div className="p-2 rounded-md bg-muted">
                        <Calendar className="h-4 w-4 text-muted-foreground" />
                      </div>
                      <div className="flex-1 min-w-0">
                        <p className="text-sm font-medium text-muted-foreground">
                          Last Modified
                        </p>
                        <p className="text-base font-mono break-all">
                          {formatDate(
                            (entityInfo as any).metadata.last_modified
                          )}
                        </p>
                      </div>
                    </div>
                  )}

                  {/* ETag */}
                  {(entityInfo as any).metadata.etag && (
                    <div className="flex items-start gap-3">
                      <div className="p-2 rounded-md bg-muted">
                        <Hash className="h-4 w-4 text-muted-foreground" />
                      </div>
                      <div className="flex-1 min-w-0">
                        <p className="text-sm font-medium text-muted-foreground">
                          ETag
                        </p>
                        <p className="text-base font-mono break-all">
                          {(entityInfo as any).metadata.etag}
                        </p>
                      </div>
                    </div>
                  )}

                  {/* Storage Class */}
                  {(entityInfo as any).metadata.storage_class && (
                    <div className="flex items-start gap-3">
                      <div className="p-2 rounded-md bg-muted">
                        <Package className="h-4 w-4 text-muted-foreground" />
                      </div>
                      <div className="flex-1 min-w-0">
                        <p className="text-sm font-medium text-muted-foreground">
                          Storage Class
                        </p>
                        <p className="text-base font-mono break-all">
                          {(entityInfo as any).metadata.storage_class}
                        </p>
                      </div>
                    </div>
                  )}
                </div>
              </CardContent>
            )}

          {!isObjectStore() && showSchema && entityInfo.fields && (
            <CardContent>
              <div className="space-y-2">
                <h3 className="font-medium text-sm mb-3">Schema</h3>
                <div className="rounded-md border">
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="border-b bg-muted/50">
                        <th className="text-left p-3 font-medium">Field</th>
                        <th className="text-left p-3 font-medium">Type</th>
                        <th className="text-left p-3 font-medium">Nullable</th>
                      </tr>
                    </thead>
                    <tbody>
                      {entityInfo.fields.map((field: FieldResponse) => (
                        <tr key={field.name} className="border-b last:border-0">
                          <td className="p-3 font-mono">{field.name}</td>
                          <td className="p-3">
                            <Badge variant="outline">{field.field_type}</Badge>
                          </td>
                          <td className="p-3">
                            <Badge
                              variant={field.nullable ? 'secondary' : 'default'}
                            >
                              {field.nullable ? 'Yes' : 'No'}
                            </Badge>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            </CardContent>
          )}
        </Card>
      )}

      {/* Data Table - Only show for non-S3-objects */}
      {!(isObjectStore() && entityInfo?.entity_type === 'object') && (
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div>
                <CardTitle className="flex items-center gap-2">
                  Data
                  {hasFilterSupport && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => setIsFilterExpanded(!isFilterExpanded)}
                      className="h-7 px-2"
                    >
                      {isFilterExpanded ? (
                        <>
                          <ChevronDown className="h-4 w-4" />
                          <span className="text-xs ml-1">Hide Filter</span>
                        </>
                      ) : (
                        <>
                          <ChevronRight className="h-4 w-4" />
                          <span className="text-xs ml-1">Show Filter</span>
                        </>
                      )}
                    </Button>
                  )}
                </CardTitle>
                {queryResult && (
                  <CardDescription>
                    Showing {queryResult.returned_count} of{' '}
                    {queryResult.total_count || '?'}{' '}
                    {isObjectStore() ? 'objects' : 'rows'}
                    {appliedFilter !== undefined && ' (filtered)'} • Execution
                    time: {queryResult.execution_time_ms}ms
                  </CardDescription>
                )}
              </div>
              {queryLoading && (
                <div className="flex items-center gap-2 text-sm text-muted-foreground">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  <span>Loading...</span>
                </div>
              )}
            </div>

            {/* Show error if query failed */}
            {queryError && errorTitle && errorDetail && (
              <Alert variant="destructive" className="mt-4">
                <AlertCircle className="h-4 w-4" />
                <AlertDescription>
                  <div className="space-y-1">
                    <p className="font-semibold">{errorTitle}</p>
                    <p className="text-sm">{errorDetail}</p>
                  </div>
                </AlertDescription>
              </Alert>
            )}
            {/* Filter Input - Only show if filtering is supported and expanded */}
            {hasFilterSupport && isFilterExpanded && (
              <div className="mt-4 space-y-3">
                {/* Show schema-based filter builder if filter_schema exists */}
                {hasFilterSchema && explorerSupport?.filter_schema ? (
                  <DynamicFilterBuilder
                    schema={explorerSupport.filter_schema}
                    formData={filterFormData}
                    onFormDataChange={onFilterFormDataChange}
                    onApplyFilter={onApplyFilter}
                  />
                ) : (
                  /* Show simple text input for SQL WHERE clause */
                  <div className="relative flex-1">
                    <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
                    <input
                      type="text"
                      placeholder={
                        hasSqlCapability
                          ? 'Filter data (SQL WHERE clause)...'
                          : 'Filter data (server-side search)...'
                      }
                      value={dataFilterInput}
                      onChange={(e) => onDataFilterInputChange(e.target.value)}
                      onKeyDown={(e) => {
                        // Apply filter on Enter (with or without Ctrl/Cmd)
                        if (e.key === 'Enter') {
                          onApplyFilter()
                        }
                      }}
                      className="w-full pl-10 pr-4 py-2.5 text-sm border rounded-md bg-background focus:outline-none focus:ring-2 focus:ring-ring"
                    />
                  </div>
                )}

                {/* Action buttons */}
                <div className="flex gap-2">
                  <Button
                    onClick={onApplyFilter}
                    disabled={
                      hasFilterSchema
                        ? Object.keys(filterFormData).length === 0
                        : !dataFilterInput.trim()
                    }
                    size="default"
                    className="px-6"
                  >
                    Apply Filter
                  </Button>
                  {appliedFilter !== undefined && (
                    <Button
                      onClick={onClearFilter}
                      variant="outline"
                      size="default"
                      className="gap-2"
                    >
                      <X className="h-4 w-4" />
                      Clear
                    </Button>
                  )}
                </div>
              </div>
            )}
            {/* Show info badge about capabilities */}
            {explorerSupport && (
              <div className="flex gap-2 mt-3">
                {explorerSupport.capabilities.map((capability) => (
                  <Badge
                    key={capability}
                    variant="secondary"
                    className="text-xs"
                  >
                    {capability.toUpperCase()}
                  </Badge>
                ))}
              </div>
            )}
          </CardHeader>
          <CardContent>
            {queryResult && queryResult.rows && queryResult.rows.length > 0 ? (
              <>
                <div className="rounded-md border overflow-x-auto">
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="border-b bg-muted/50">
                        {queryResult.fields?.map((field: FieldResponse) => (
                          <th
                            key={field.name}
                            className="text-left p-3 font-medium whitespace-nowrap"
                          >
                            <button
                              onClick={() => onSort(field.name)}
                              className="flex items-center gap-2 hover:text-foreground transition-colors group w-full"
                            >
                              <span>{field.name}</span>
                              {dataSortField === field.name ? (
                                dataSortOrder === 'asc' ? (
                                  <SortAsc className="h-4 w-4" />
                                ) : (
                                  <SortDesc className="h-4 w-4" />
                                )
                              ) : (
                                <ArrowUpDown className="h-4 w-4 opacity-0 group-hover:opacity-50 transition-opacity" />
                              )}
                            </button>
                          </th>
                        ))}
                      </tr>
                    </thead>
                    <tbody>
                      {queryResult.rows.map((row: any, rowIndex: number) => (
                        <tr
                          key={rowIndex}
                          className="border-b last:border-0 hover:bg-muted/30"
                        >
                          {queryResult.fields?.map((field: FieldResponse) => (
                            <td
                              key={field.name}
                              className="p-3 font-mono text-xs max-w-xs truncate"
                              title={String(row[field.name])}
                            >
                              {row[field.name] !== null &&
                              row[field.name] !== undefined
                                ? String(row[field.name])
                                : '-'}
                            </td>
                          ))}
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>

                {/* Pagination */}
                <div className="flex items-center justify-between mt-4">
                  <div className="text-sm text-muted-foreground flex items-center gap-2">
                    <span>
                      Page {page} • Rows {(page - 1) * pageSize + 1} -{' '}
                      {(page - 1) * pageSize + queryResult.returned_count}
                    </span>
                    {appliedFilter !== undefined && (
                      <Badge variant="secondary" className="text-xs">
                        Filtered
                      </Badge>
                    )}
                  </div>
                  <div className="flex items-center gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={page === 1}
                      onClick={() => onPageChange(page - 1)}
                    >
                      Previous
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={
                        !queryResult || queryResult.returned_count < pageSize
                      }
                      onClick={() => onPageChange(page + 1)}
                    >
                      Next
                    </Button>
                  </div>
                </div>
              </>
            ) : (
              <div className="text-center py-8 text-sm text-muted-foreground">
                {appliedFilter !== undefined
                  ? 'No results match your filter'
                  : 'No data found'}
              </div>
            )}
          </CardContent>
        </Card>
      )}
    </div>
  )
}
