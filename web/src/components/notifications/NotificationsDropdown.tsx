import {
  listNotificationsOptions,
  markAllNotificationsReadMutation,
  markNotificationReadMutation,
  markNotificationsBulkMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { NotificationDto } from '@/api/client/types.gen'
import { cn } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  Bell,
  Check,
  CheckCheck,
  ChevronLeft,
  ChevronRight,
  Circle,
  Loader2,
} from 'lucide-react'
import React, { useState } from 'react'
import { toast } from 'sonner'
import { Badge } from '../ui/badge'
import { Button } from '../ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from '../ui/dropdown-menu'
import { ScrollArea } from '../ui/scroll-area'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../ui/tabs'
import { TimeAgo } from '../utils/TimeAgo'

export function NotificationsDropdown() {
  const [isOpen, setIsOpen] = useState(false)
  const [activeTab, setActiveTab] = useState<'unread' | 'read'>('unread')
  const [unreadPage, setUnreadPage] = useState(1)
  const [readPage, setReadPage] = useState(1)
  const queryClient = useQueryClient()

  // Fetch unread notifications
  const {
    data: unreadData,
    isLoading: isLoadingUnread,
    error: unreadError,
  } = useQuery({
    ...listNotificationsOptions({
      query: {
        page: unreadPage,
        per_page: 10,
        is_read: false,
      },
    }),
    refetchInterval: 30000, // Refetch every 30 seconds
  })

  // Fetch read notifications
  const {
    data: readData,
    isLoading: isLoadingRead,
    error: readError,
  } = useQuery({
    ...listNotificationsOptions({
      query: {
        page: readPage,
        per_page: 10,
        is_read: true,
      },
    }),
    enabled: activeTab === 'read', // Only fetch when read tab is active
  })

  const unreadNotifications = unreadData?.notifications || []
  const readNotifications = readData?.notifications || []
  const unreadCount = unreadData?.unread_count || 0
  const unreadTotalPages = unreadData
    ? Math.ceil((unreadData.total || 0) / 10)
    : 1
  const readTotalPages = readData ? Math.ceil((readData.total || 0) / 10) : 1

  // Mark single notification as read
  const markAsReadMutation = useMutation({
    ...markNotificationReadMutation(),
    meta: {
      errorTitle: 'Failed to mark notification as read',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['listNotifications'] })
      toast.success('Notification marked as read')
    },
  })

  // Mark all as read
  const markAllAsReadMutation = useMutation({
    ...markAllNotificationsReadMutation(),
    meta: {
      errorTitle: 'Failed to mark all notifications as read',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['listNotifications'] })
      toast.success('All notifications marked as read')
    },
  })

  // Mark multiple notifications
  const _markBulkMutation = useMutation({
    ...markNotificationsBulkMutation(),
    meta: {
      errorTitle: 'Failed to update notifications',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['listNotifications'] })
      toast.success('Notifications updated')
    },
  })

  const handleMarkAsRead = (notification: NotificationDto) => {
    if (!notification.is_read) {
      markAsReadMutation.mutate({
        path: { id: notification.id },
        body: { is_read: true },
      })
    }
  }

  const handleMarkAsUnread = (notification: NotificationDto) => {
    if (notification.is_read) {
      markAsReadMutation.mutate({
        path: { id: notification.id },
        body: { is_read: false },
      })
    }
  }

  const handleMarkAllAsRead = () => {
    markAllAsReadMutation.mutate({})
  }

  const handleToggleRead = (
    e: React.MouseEvent,
    notification: NotificationDto
  ) => {
    e.stopPropagation()
    e.preventDefault()
    if (notification.is_read) {
      handleMarkAsUnread(notification)
    } else {
      handleMarkAsRead(notification)
    }
  }

  const renderNotification = (notification: NotificationDto) => (
    <div
      key={notification.id}
      className={cn(
        'px-4 py-3 hover:bg-muted/50 transition-colors group',
        !notification.is_read && 'bg-blue-50/50 dark:bg-blue-950/20'
      )}
    >
      <div className="flex items-start gap-3">
        <div className="mt-1">
          {!notification.is_read ? (
            <Circle className="h-2 w-2 fill-primary text-primary" />
          ) : (
            <div className="h-2 w-2" />
          )}
        </div>
        <div className="flex-1 space-y-1">
          <div className="flex items-start justify-between gap-2">
            <div className="flex items-start gap-2 flex-1">
              {getNotificationIcon(notification)}
              <p className="text-sm leading-relaxed">{notification.message}</p>
            </div>
            <Button
              variant="ghost"
              size="sm"
              className="h-auto p-1 opacity-0 group-hover:opacity-100 transition-opacity"
              onClick={(e) => handleToggleRead(e, notification)}
              title={notification.is_read ? 'Mark as unread' : 'Mark as read'}
            >
              {notification.is_read ? (
                <Circle className="h-3 w-3" />
              ) : (
                <Check className="h-3 w-3" />
              )}
            </Button>
          </div>
          <p className="text-xs text-muted-foreground">
            <TimeAgo date={notification.created_at} />
          </p>
        </div>
      </div>
    </div>
  )

  const renderPagination = (
    currentPage: number,
    totalPages: number,
    onPageChange: (page: number) => void
  ) => (
    <div className="flex items-center justify-between px-4 py-2 border-t">
      <Button
        variant="ghost"
        size="sm"
        onClick={() => onPageChange(currentPage - 1)}
        disabled={currentPage === 1}
      >
        <ChevronLeft className="h-4 w-4" />
        Previous
      </Button>
      <span className="text-xs text-muted-foreground">
        Page {currentPage} of {totalPages}
      </span>
      <Button
        variant="ghost"
        size="sm"
        onClick={() => onPageChange(currentPage + 1)}
        disabled={currentPage === totalPages}
      >
        Next
        <ChevronRight className="h-4 w-4" />
      </Button>
    </div>
  )

  const getNotificationIcon = (notification: NotificationDto) => {
    // You can customize this based on notification type/category
    if (
      notification.message.toLowerCase().includes('error') ||
      notification.message.toLowerCase().includes('fail')
    ) {
      return <AlertCircle className="h-4 w-4 text-destructive" />
    }
    if (
      notification.message.toLowerCase().includes('success') ||
      notification.message.toLowerCase().includes('complete')
    ) {
      return <Check className="h-4 w-4 text-green-600" />
    }
    return <Bell className="h-4 w-4" />
  }

  return (
    <DropdownMenu open={isOpen} onOpenChange={setIsOpen}>
      <DropdownMenuTrigger asChild>
        <Button variant="outline" size="icon" className="relative">
          <Bell className="h-4 w-4" />
          {unreadCount > 0 && (
            <Badge
              variant="destructive"
              className="absolute -top-1 -right-1 h-5 w-5 p-0 flex items-center justify-center text-xs"
            >
              {unreadCount > 99 ? '99+' : unreadCount}
            </Badge>
          )}
          <span className="sr-only">Notifications</span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-[420px] p-0">
        <div className="flex items-center justify-between px-4 py-3 border-b">
          <DropdownMenuLabel className="p-0">Notifications</DropdownMenuLabel>
          {unreadCount > 0 && activeTab === 'unread' && (
            <Button
              variant="ghost"
              size="sm"
              onClick={handleMarkAllAsRead}
              disabled={markAllAsReadMutation.isPending}
              className="h-auto py-1 px-2 text-xs"
            >
              {markAllAsReadMutation.isPending ? (
                <Loader2 className="h-3 w-3 animate-spin mr-1" />
              ) : (
                <CheckCheck className="h-3 w-3 mr-1" />
              )}
              Mark all as read
            </Button>
          )}
        </div>

        <Tabs
          value={activeTab}
          onValueChange={(v) => setActiveTab(v as 'unread' | 'read')}
          className="w-full"
        >
          <TabsList className="w-full rounded-none border-b">
            <TabsTrigger value="unread" className="flex-1">
              Unread
              {unreadCount > 0 && (
                <Badge variant="secondary" className="ml-2 h-5 px-1">
                  {unreadCount}
                </Badge>
              )}
            </TabsTrigger>
            <TabsTrigger value="read" className="flex-1">
              Read
            </TabsTrigger>
          </TabsList>

          {/* Unread Tab */}
          <TabsContent value="unread" className="m-0">
            <ScrollArea className="h-[400px]">
              {isLoadingUnread ? (
                <div className="flex items-center justify-center py-8">
                  <Loader2 className="h-6 w-6 animate-spin" />
                </div>
              ) : unreadError ? (
                <div className="flex flex-col items-center justify-center py-8 px-4">
                  <AlertCircle className="h-8 w-8 text-muted-foreground mb-2" />
                  <p className="text-sm text-muted-foreground text-center">
                    Failed to load notifications
                  </p>
                  <Button
                    variant="outline"
                    size="sm"
                    className="mt-2"
                    onClick={() =>
                      queryClient.invalidateQueries({
                        queryKey: ['listNotifications'],
                      })
                    }
                  >
                    Try again
                  </Button>
                </div>
              ) : unreadNotifications.length === 0 ? (
                <div className="flex flex-col items-center justify-center py-8">
                  <Bell className="h-8 w-8 text-muted-foreground mb-2" />
                  <p className="text-sm text-muted-foreground">
                    No unread notifications
                  </p>
                </div>
              ) : (
                <div className="divide-y">
                  {unreadNotifications.map(renderNotification)}
                </div>
              )}
            </ScrollArea>
            {unreadTotalPages > 1 &&
              renderPagination(unreadPage, unreadTotalPages, setUnreadPage)}
          </TabsContent>

          {/* Read Tab */}
          <TabsContent value="read" className="m-0">
            <ScrollArea className="h-[400px]">
              {isLoadingRead ? (
                <div className="flex items-center justify-center py-8">
                  <Loader2 className="h-6 w-6 animate-spin" />
                </div>
              ) : readError ? (
                <div className="flex flex-col items-center justify-center py-8 px-4">
                  <AlertCircle className="h-8 w-8 text-muted-foreground mb-2" />
                  <p className="text-sm text-muted-foreground text-center">
                    Failed to load notifications
                  </p>
                  <Button
                    variant="outline"
                    size="sm"
                    className="mt-2"
                    onClick={() =>
                      queryClient.invalidateQueries({
                        queryKey: ['listNotifications'],
                      })
                    }
                  >
                    Try again
                  </Button>
                </div>
              ) : readNotifications.length === 0 ? (
                <div className="flex flex-col items-center justify-center py-8">
                  <CheckCheck className="h-8 w-8 text-muted-foreground mb-2" />
                  <p className="text-sm text-muted-foreground">
                    No read notifications
                  </p>
                </div>
              ) : (
                <div className="divide-y">
                  {readNotifications.map(renderNotification)}
                </div>
              )}
            </ScrollArea>
            {readTotalPages > 1 &&
              renderPagination(readPage, readTotalPages, setReadPage)}
          </TabsContent>
        </Tabs>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
