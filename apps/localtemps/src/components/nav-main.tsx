"use client"

import { type LucideIcon } from "lucide-react"
import { type NavPage } from "@/components/app-sidebar"

import {
  SidebarGroup,
  SidebarGroupLabel,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from "@/components/ui/sidebar"

interface NavItem {
  title: string
  page: NavPage
  icon?: LucideIcon
  isActive?: boolean
}

interface NavMainProps {
  items: NavItem[]
  onNavigate: (page: NavPage) => void
}

export function NavMain({ items, onNavigate }: NavMainProps) {
  return (
    <SidebarGroup>
      <SidebarGroupLabel>Platform</SidebarGroupLabel>
      <SidebarMenu>
        {items.map((item) => (
          <SidebarMenuItem key={item.title}>
            <SidebarMenuButton
              tooltip={item.title}
              isActive={item.isActive}
              onClick={() => onNavigate(item.page)}
            >
              {item.icon && <item.icon />}
              <span>{item.title}</span>
            </SidebarMenuButton>
          </SidebarMenuItem>
        ))}
      </SidebarMenu>
    </SidebarGroup>
  )
}
