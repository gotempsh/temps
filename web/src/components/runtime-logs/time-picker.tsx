import { Button } from '@/components/ui/button'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { ScrollArea } from '@/components/ui/scroll-area'
import { cn } from '@/lib/utils'
import { Clock } from 'lucide-react'

interface TimePickerProps {
  value?: string
  onChange: (value: string) => void
  disabled?: boolean
}

export function TimePicker({
  value = '00:00',
  onChange,
  disabled,
}: TimePickerProps) {
  const hours = Array.from({ length: 24 }, (_, i) =>
    i.toString().padStart(2, '0')
  )
  const minutes = Array.from({ length: 60 }, (_, i) =>
    i.toString().padStart(2, '0')
  )

  const [currentHour, currentMinute] = value.split(':')

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          variant="outline"
          disabled={disabled}
          className={cn(
            'w-[120px] justify-start text-left font-normal',
            !value && 'text-muted-foreground'
          )}
        >
          <Clock className="mr-2 h-4 w-4" />
          {value}
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-[280px] p-4">
        <div className="flex gap-4">
          <div className="flex-1 space-y-1">
            <div className="text-xs font-medium">Hours</div>
            <ScrollArea className="h-[200px] rounded-md border">
              <div className="p-2">
                {hours.map((hour) => (
                  <div
                    key={hour}
                    className={cn(
                      'cursor-pointer rounded-md px-3 py-2 text-sm hover:bg-accent',
                      hour === currentHour && 'bg-accent'
                    )}
                    onClick={() => onChange(`${hour}:${currentMinute}`)}
                  >
                    {hour}
                  </div>
                ))}
              </div>
            </ScrollArea>
          </div>
          <div className="flex-1 space-y-1">
            <div className="text-xs font-medium">Minutes</div>
            <ScrollArea className="h-[200px] rounded-md border">
              <div className="p-2">
                {minutes.map((minute) => (
                  <div
                    key={minute}
                    className={cn(
                      'cursor-pointer rounded-md px-3 py-2 text-sm hover:bg-accent',
                      minute === currentMinute && 'bg-accent'
                    )}
                    onClick={() => onChange(`${currentHour}:${minute}`)}
                  >
                    {minute}
                  </div>
                ))}
              </div>
            </ScrollArea>
          </div>
        </div>
      </PopoverContent>
    </Popover>
  )
}
