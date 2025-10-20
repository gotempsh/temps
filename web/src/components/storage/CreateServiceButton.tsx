import { useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Plus, ChevronDown } from 'lucide-react'
import { CreateServiceDialog } from '@/components/storage/CreateServiceDialog'
import { ServiceLogo } from '@/components/ui/service-logo'
import { ServiceType } from '@/api/client/types.gen'

const SERVICE_TYPES = [
  {
    id: 'postgres',
    name: 'PostgreSQL',
    description: 'Reliable Relational Database',
    icon: <ServiceLogo service="postgres" />,
  },
  {
    id: 'redis',
    name: 'Redis',
    description: 'In-Memory Data Store',
    icon: <ServiceLogo service="redis" />,
  },
  {
    id: 's3',
    name: 'S3',
    description: 'Object Storage',
    icon: <ServiceLogo service="s3" />,
  },
  {
    id: 'libsql',
    name: 'LibSQL',
    description: 'SQLite-compatible Database',
    icon: <ServiceLogo service="libsql" />,
  },
] as {
  id: ServiceType
  name: string
  description: string
  icon: React.ReactNode
}[]

export function CreateServiceButton({ onSuccess }: { onSuccess?: () => void }) {
  const queryClient = useQueryClient()
  const [isCreateDialogOpen, setIsCreateDialogOpen] = useState(false)
  const [selectedServiceType, setSelectedServiceType] =
    useState<ServiceType | null>(null)

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button className="gap-2">
            <Plus className="h-4 w-4" />
            Create Service
            <ChevronDown className="h-4 w-4 ml-1" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-[240px]">
          {SERVICE_TYPES.map((type) => (
            <DropdownMenuItem
              key={type.id}
              onClick={() => {
                setSelectedServiceType(type.id)
                setIsCreateDialogOpen(true)
              }}
              className="flex items-start gap-3 py-3"
            >
              <ServiceLogo service={type.id} size={24} />
              <div className="flex flex-col">
                <span className="font-medium">{type.name}</span>
                <span className="text-xs text-muted-foreground">
                  {type.description}
                </span>
              </div>
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>

      {selectedServiceType && (
        <CreateServiceDialog
          open={isCreateDialogOpen}
          onOpenChange={(open) => {
            setIsCreateDialogOpen(open)
            if (!open) {
              setSelectedServiceType(null)
            }
          }}
          onSuccess={() => {
            setIsCreateDialogOpen(false)
            setSelectedServiceType(null)
            queryClient.invalidateQueries({ queryKey: ['services'] })
            onSuccess?.()
          }}
          serviceType={selectedServiceType}
        />
      )}
    </>
  )
}
