"use client"

import { Badge } from "@/components/ui/badge"
import {
  SidebarMenu,
  SidebarMenuItem,
  SidebarMenuButton,
} from "@/components/ui/sidebar"

interface NavStatusProps {
  apiRunning: boolean
}

export function NavStatus({ apiRunning }: NavStatusProps) {
  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <SidebarMenuButton
          size="lg"
          className="cursor-default hover:bg-transparent"
        >
          <div className="flex items-center gap-2">
            <div
              className={`h-2 w-2 rounded-full ${
                apiRunning ? "bg-green-500" : "bg-muted-foreground"
              }`}
            />
            <span className="text-sm font-medium">
              API {apiRunning ? "Running" : "Stopped"}
            </span>
          </div>
          <Badge
            variant={apiRunning ? "default" : "secondary"}
            className="ml-auto"
          >
            {apiRunning ? "Online" : "Offline"}
          </Badge>
        </SidebarMenuButton>
      </SidebarMenuItem>
    </SidebarMenu>
  )
}
