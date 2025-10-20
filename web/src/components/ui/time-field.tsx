'use client'

import { Input } from '@/components/ui/input'
import { format } from 'date-fns'
import { useEffect, useState } from 'react'

interface TimeFieldProps {
  value?: Date | null
  onChange?: (date: Date) => void
}

export function TimeField({ value, onChange }: TimeFieldProps) {
  const [time, setTime] = useState(() => (value ? format(value, 'HH:mm') : ''))

  useEffect(() => {
    if (value) {
      setTime(format(value, 'HH:mm'))
    }
  }, [value])

  const handleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const newTime = e.target.value
    setTime(newTime)

    if (newTime && /^([0-1]?[0-9]|2[0-3]):[0-5][0-9]$/.test(newTime)) {
      const [hours, minutes] = newTime.split(':').map(Number)
      const date = new Date()
      date.setHours(hours)
      date.setMinutes(minutes)
      date.setSeconds(0)
      date.setMilliseconds(0)
      onChange?.(date)
    }
  }

  return (
    <Input
      type="time"
      value={time}
      onChange={handleChange}
      className="w-full"
    />
  )
}
