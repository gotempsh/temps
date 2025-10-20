import { DateRangePicker } from '@/components/ui/date-range-picker'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'

interface FilterBarProps {
  onStartDateChange: (date: Date | undefined) => void
  onEndDateChange: (date: Date | undefined) => void
  onTailLinesChange: (lines: number) => void
  startDate?: Date
  endDate?: Date
  tailLines?: number
}

export function FilterBar({
  onStartDateChange,
  onEndDateChange,
  onTailLinesChange,
  startDate,
  endDate,
  tailLines,
}: FilterBarProps) {
  return (
    <div className="flex flex-col sm:flex-row gap-4">
      <div className="grid gap-2">
        <Label className="text-foreground">Date Range</Label>
        <DateRangePicker
          date={{ from: startDate, to: endDate }}
          onDateChange={(date) => {
            onStartDateChange(date?.from)
            onEndDateChange(date?.to)
          }}
        />
      </div>

      <div className="grid gap-2">
        <Label className="text-foreground">Tail Lines</Label>
        <Select
          value={tailLines?.toString()}
          onValueChange={(value) => onTailLinesChange(Number(value))}
        >
          <SelectTrigger className="w-[180px] bg-background">
            <SelectValue placeholder="Number of lines" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="100">Last 100 lines</SelectItem>
            <SelectItem value="500">Last 500 lines</SelectItem>
            <SelectItem value="1000">Last 1000 lines</SelectItem>
            <SelectItem value="5000">Last 5000 lines</SelectItem>
          </SelectContent>
        </Select>
      </div>
    </div>
  )
}
