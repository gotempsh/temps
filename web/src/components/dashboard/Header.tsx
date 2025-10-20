import { ThemeToggle } from '@/components/theme/ThemeToggle'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { FolderPlus, GitBranch, Globe, Key, Plus } from 'lucide-react'
import React from 'react'
import { Link, useNavigate } from 'react-router-dom'
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from '../ui/breadcrumb'
import { Button } from '../ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '../ui/dropdown-menu'
import { Separator } from '../ui/separator'
import { SidebarTrigger } from '../ui/sidebar'

export function Header() {
  const { breadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()

  const handleCreateProject = () => {
    navigate('/projects/new')
  }

  const handleProvisionDomain = () => {
    navigate('/domains')
  }

  const handleCreateApiKey = () => {
    navigate('/keys/new')
  }

  const handleAddGitProvider = () => {
    navigate('/git-sources/add')
  }

  return (
    <header className="flex h-16 shrink-0 items-center gap-2 border-b px-4">
      <div className="flex justify-between w-full">
        <div className="flex items-center">
          <SidebarTrigger className="-ml-1" />
          <Separator orientation="vertical" className="mr-2 h-4" />
          <Breadcrumb>
            <BreadcrumbList>
              {breadcrumbs.map((item, index) => (
                <React.Fragment key={index}>
                  <BreadcrumbItem>
                    {index < breadcrumbs.length - 1 ? (
                      <BreadcrumbLink asChild href={item.href ?? '#'}>
                        <Link to={item.href ?? '#'}>{item.label}</Link>
                      </BreadcrumbLink>
                    ) : (
                      <BreadcrumbPage>{item.label}</BreadcrumbPage>
                    )}
                  </BreadcrumbItem>
                  {index < breadcrumbs.length - 1 && <BreadcrumbSeparator />}
                </React.Fragment>
              ))}
            </BreadcrumbList>
          </Breadcrumb>
        </div>
        <div className="ml-auto flex items-center space-x-2">
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" size="icon">
                <Plus className="h-4 w-4" />
                <span className="sr-only">Create new</span>
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-[200px]">
              <DropdownMenuLabel>Quick Actions</DropdownMenuLabel>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={handleCreateProject}>
                <FolderPlus className="mr-2 h-4 w-4" />
                Create Project
              </DropdownMenuItem>
              <DropdownMenuItem onClick={handleProvisionDomain}>
                <Globe className="mr-2 h-4 w-4" />
                Provision Domain
              </DropdownMenuItem>
              <DropdownMenuItem onClick={handleCreateApiKey}>
                <Key className="mr-2 h-4 w-4" />
                Create API Key
              </DropdownMenuItem>
              <DropdownMenuItem onClick={handleAddGitProvider}>
                <GitBranch className="mr-2 h-4 w-4" />
                Add Git Provider
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          {/* <NotificationsDropdown /> */}
          <ThemeToggle />
        </div>
      </div>
    </header>
  )
}
