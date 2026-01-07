"use client"

import * as React from "react"
import {
  Activity,
  Database,
  HardDrive,
  Settings,
} from "lucide-react"

import { NavMain } from "@/components/nav-main"
import { NavStatus } from "@/components/nav-status"
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarRail,
} from "@/components/ui/sidebar"

export type NavPage = 'services' | 'analytics' | 'settings'

interface AppSidebarProps extends React.ComponentProps<typeof Sidebar> {
  activePage: NavPage
  onNavigate: (page: NavPage) => void
  apiRunning: boolean
}

export function AppSidebar({ activePage, onNavigate, apiRunning, ...props }: AppSidebarProps) {
  const navMain = [
    {
      title: "Services",
      page: "services" as NavPage,
      icon: Database,
      isActive: activePage === "services",
    },
    {
      title: "Analytics",
      page: "analytics" as NavPage,
      icon: Activity,
      isActive: activePage === "analytics",
    },
    {
      title: "Settings",
      page: "settings" as NavPage,
      icon: Settings,
      isActive: activePage === "settings",
    },
  ]

  return (
    <Sidebar collapsible="icon" {...props}>
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              size="lg"
              className="data-[state=open]:bg-sidebar-accent data-[state=open]:text-sidebar-accent-foreground"
            >
              <div className="flex aspect-square size-8 items-center justify-center rounded-lg bg-sidebar-primary text-sidebar-primary-foreground">
                <HardDrive className="size-4" />
              </div>
              <div className="grid flex-1 text-left text-sm leading-tight">
                <span className="truncate font-semibold">LocalTemps</span>
                <span className="truncate text-xs">Local Development</span>
              </div>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      <SidebarContent>
        <NavMain items={navMain} onNavigate={onNavigate} />
      </SidebarContent>
      <SidebarFooter>
        <NavStatus apiRunning={apiRunning} />
      </SidebarFooter>
      <SidebarRail />
    </Sidebar>
  )
}
