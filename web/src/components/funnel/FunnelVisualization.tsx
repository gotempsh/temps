import { StepConversionResponse } from '@/api/client/types.gen'
import { cn } from '@/lib/utils'
import {
  ChevronRight,
  TrendingDown,
  TrendingUp,
  Users,
  UserCheck,
  Clock,
  DollarSign,
} from 'lucide-react'
import * as React from 'react'

interface FunnelVisualizationProps {
  totalEntries: number
  stepConversions: StepConversionResponse[]
  conversionRate: number
  averageCompletionTime: number
  stepValue?: number // Value per step completion in dollars
}

interface CumulativeDataItem extends StepConversionResponse {
  dropoff: number
  dropoffRate: number
  conversionFromStart: number
  conversionFromPrevious: number
  previousCompletions: number
  value: number
}

interface ViewProps {
  totalEntries: number
  stepConversions: StepConversionResponse[]
  conversionRate: number
  averageCompletionTime: number
  stepValue: number
  maxValue: number
  cumulativeData: CumulativeDataItem[]
}

// Modern Funnel View (Default)
function FunnelView({
  totalEntries,
  stepConversions,
  conversionRate,
  stepValue,
  maxValue,
  cumulativeData,
  averageCompletionTime,
}: ViewProps) {
  return (
    <div className="relative">
      {/* Header Metrics Bar */}
      <div className="grid grid-cols-4 gap-4 mb-8 p-4 bg-muted/30 rounded-lg">
        <div className="text-center">
          <div className="flex items-center justify-center gap-2 text-muted-foreground mb-1">
            <Users className="h-4 w-4" />
            <span className="text-xs uppercase tracking-wider">
              Total Entries
            </span>
          </div>
          <div className="text-2xl font-bold">
            {totalEntries.toLocaleString()}
          </div>
        </div>
        <div className="text-center">
          <div className="flex items-center justify-center gap-2 text-muted-foreground mb-1">
            <UserCheck className="h-4 w-4" />
            <span className="text-xs uppercase tracking-wider">
              Completions
            </span>
          </div>
          <div className="text-2xl font-bold">
            {stepConversions[
              stepConversions.length - 1
            ]?.completions.toLocaleString() || 0}
          </div>
        </div>
        <div className="text-center">
          <div className="flex items-center justify-center gap-2 text-muted-foreground mb-1">
            <TrendingUp className="h-4 w-4" />
            <span className="text-xs uppercase tracking-wider">Conversion</span>
          </div>
          <div className="text-2xl font-bold text-green-600">
            {conversionRate.toFixed(1)}%
          </div>
        </div>
        {stepValue > 0 && (
          <div className="text-center">
            <div className="flex items-center justify-center gap-2 text-muted-foreground mb-1">
              <DollarSign className="h-4 w-4" />
              <span className="text-xs uppercase tracking-wider">
                Total Value
              </span>
            </div>
            <div className="text-2xl font-bold">
              $
              {(
                stepValue *
                (stepConversions[stepConversions.length - 1]?.completions || 0)
              ).toLocaleString()}
            </div>
          </div>
        )}
      </div>

      {/* Funnel Visualization */}
      <div className="space-y-1">
        {/* Steps */}
        {cumulativeData.map((step, index) => {
          const width = (step.completions / maxValue) * 100
          const isFirstStep = index === 0
          const isLastStep = index === cumulativeData.length - 1

          // Dynamic color based on performance
          const performanceColor =
            step.conversionFromPrevious >= 70
              ? 'from-green-500 to-green-600'
              : step.conversionFromPrevious >= 40
                ? 'from-yellow-500 to-yellow-600'
                : 'from-red-500 to-red-600'

          // For first step, use different colors
          const stepColor = isFirstStep
            ? 'from-blue-500 to-blue-600'
            : performanceColor

          return (
            <div key={step.step_id} className="relative">
              {/* Connector with drop-off - only show after first step */}
              {!isFirstStep && (
                <div className="relative h-8 flex items-center justify-center">
                  <div className="absolute left-1/2 transform -translate-x-1/2 flex items-center gap-4">
                    <div className="flex items-center gap-2 px-3 py-1 bg-red-50 dark:bg-red-950/30 rounded-full">
                      <TrendingDown className="h-3 w-3 text-red-500" />
                      <span className="text-xs font-medium text-red-600 dark:text-red-400">
                        -{step.dropoff.toLocaleString()} (
                        {step.dropoffRate.toFixed(1)}%)
                      </span>
                    </div>
                  </div>
                </div>
              )}

              {/* Step */}
              <div
                className={cn(
                  'relative mx-auto group',
                  isFirstStep && 'rounded-t-2xl',
                  isLastStep && 'rounded-b-2xl'
                )}
                style={{
                  width: isFirstStep ? '100%' : `${Math.max(width, 40)}%`,
                }}
              >
                <div
                  className={cn(
                    'relative bg-gradient-to-r p-6 shadow-lg',
                    stepColor,
                    isFirstStep && 'rounded-t-2xl',
                    isLastStep && 'rounded-b-2xl'
                  )}
                >
                  <div className="flex items-center justify-between text-white">
                    <div className="flex-1">
                      <div className="flex items-center gap-2 mb-2">
                        <div className="w-8 h-8 bg-white/20 rounded-full flex items-center justify-center font-bold">
                          {step.step_order}
                        </div>
                        <div>
                          <div className="font-semibold text-lg">
                            {step.step_name}
                          </div>
                          <div className="text-sm opacity-90">
                            <Clock className="inline h-3 w-3 mr-1" />
                            {step.average_time_to_complete_seconds >= 60
                              ? `${Math.round(step.average_time_to_complete_seconds / 60)}m`
                              : `${Math.round(step.average_time_to_complete_seconds)}s`}
                          </div>
                        </div>
                      </div>
                    </div>
                    <div className="text-right">
                      <div className="text-2xl font-bold">
                        {step.completions.toLocaleString()}
                      </div>
                      <div className="text-sm opacity-90">
                        {isFirstStep
                          ? 'Entry Point'
                          : `${step.conversionFromStart.toFixed(1)}% of total`}
                      </div>
                      {stepValue > 0 && (
                        <div className="text-xs opacity-75 mt-1">
                          ${step.value.toLocaleString()} value
                        </div>
                      )}
                    </div>
                  </div>

                  {/* Additional stats in the card */}
                  {!isFirstStep && (
                    <div className="mt-4 pt-4 border-t border-white/20 grid grid-cols-2 gap-4 text-sm">
                      <div>
                        <div className="text-xs opacity-75">
                          From Previous Step
                        </div>
                        <div className="font-semibold">
                          {step.conversionFromPrevious.toFixed(1)}%
                        </div>
                      </div>
                      <div>
                        <div className="text-xs opacity-75">Drop-off Rate</div>
                        <div className="font-semibold">
                          {step.drop_off_rate.toFixed(1)}%
                        </div>
                      </div>
                    </div>
                  )}
                </div>
              </div>
            </div>
          )
        })}
      </div>

      {/* Summary Stats */}
      <div className="mt-8 p-4 bg-muted/30 rounded-lg">
        <div className="grid grid-cols-3 gap-4 text-center">
          <div>
            <div className="text-sm text-muted-foreground mb-1">
              Total Drop-off
            </div>
            <div className="text-xl font-bold text-red-600">
              {totalEntries -
                (cumulativeData[cumulativeData.length - 1]?.completions ||
                  0)}{' '}
              users
            </div>
          </div>
          <div>
            <div className="text-sm text-muted-foreground mb-1">
              Average Step Conversion
            </div>
            <div className="text-xl font-bold">
              {(
                cumulativeData.reduce(
                  (acc, step) => acc + step.conversionFromPrevious,
                  0
                ) / cumulativeData.length
              ).toFixed(1)}
              %
            </div>
          </div>
          <div>
            <div className="text-sm text-muted-foreground mb-1">
              Completion Time
            </div>
            <div className="text-xl font-bold">
              {averageCompletionTime >= 3600
                ? `${Math.round(averageCompletionTime / 3600)}h`
                : `${Math.round(averageCompletionTime / 60)}m`}
            </div>
          </div>
        </div>
      </div>
    </div>
  )
}

// Horizontal Pipeline View (Alternative)
function HorizontalView({ totalEntries, cumulativeData }: ViewProps) {
  return (
    <div className="overflow-x-auto pb-4">
      <div className="min-w-[800px]">
        <div className="flex items-center gap-2">
          {/* Entry */}
          <div className="flex-shrink-0">
            <div className="bg-gradient-to-br from-blue-500 to-blue-600 text-white rounded-lg p-4 w-40">
              <div className="text-xs uppercase tracking-wider opacity-90 mb-1">
                Entry
              </div>
              <div className="text-2xl font-bold">
                {totalEntries.toLocaleString()}
              </div>
              <div className="text-sm opacity-90">100%</div>
            </div>
          </div>

          {/* Steps */}
          {cumulativeData.map((step, _index) => (
            <React.Fragment key={step.step_id}>
              {/* Connector */}
              <div className="flex-shrink-0 flex flex-col items-center">
                <ChevronRight className="h-6 w-6 text-muted-foreground" />
                <div className="text-xs text-red-500 font-medium">
                  -{step.dropoff}
                </div>
              </div>

              {/* Step */}
              <div className="flex-shrink-0">
                <div
                  className={cn(
                    'rounded-lg p-4 w-40 transition-all',
                    step.conversionFromPrevious >= 70
                      ? 'bg-gradient-to-br from-green-500 to-green-600'
                      : step.conversionFromPrevious >= 40
                        ? 'bg-gradient-to-br from-yellow-500 to-yellow-600'
                        : 'bg-gradient-to-br from-red-500 to-red-600',
                    'text-white'
                  )}
                >
                  <div className="text-xs uppercase tracking-wider opacity-90 mb-1">
                    Step {step.step_order}
                  </div>
                  <div className="font-medium mb-1 text-sm">
                    {step.step_name}
                  </div>
                  <div className="text-xl font-bold">
                    {step.completions.toLocaleString()}
                  </div>
                  <div className="text-xs opacity-90">
                    {step.conversionFromStart.toFixed(1)}%
                  </div>
                </div>
              </div>
            </React.Fragment>
          ))}
        </div>
      </div>
    </div>
  )
}

export function FunnelVisualization({
  totalEntries,
  stepConversions,
  conversionRate,
  averageCompletionTime,
  stepValue = 0,
}: FunnelVisualizationProps) {
  const [viewMode, setViewMode] = React.useState<'funnel' | 'horizontal'>(
    'funnel'
  )

  // Calculate max value for scaling
  const maxValue = Math.max(
    totalEntries,
    ...stepConversions.map((s) => s.completions)
  )

  // Calculate cumulative metrics
  const cumulativeData = React.useMemo(() => {
    return stepConversions.map((step, index) => {
      const previousCompletions =
        index === 0 ? totalEntries : stepConversions[index - 1].completions
      const dropoff = previousCompletions - step.completions
      const dropoffRate =
        previousCompletions > 0 ? (dropoff / previousCompletions) * 100 : 0
      const conversionFromStart =
        totalEntries > 0 ? (step.completions / totalEntries) * 100 : 0
      const conversionFromPrevious =
        previousCompletions > 0
          ? (step.completions / previousCompletions) * 100
          : 0

      return {
        ...step,
        dropoff,
        dropoffRate,
        conversionFromStart,
        conversionFromPrevious,
        previousCompletions,
        value: stepValue * step.completions,
      }
    })
  }, [stepConversions, totalEntries, stepValue])

  const viewProps: ViewProps = {
    totalEntries,
    stepConversions,
    conversionRate,
    averageCompletionTime,
    stepValue,
    maxValue,
    cumulativeData,
  }

  return (
    <div className="space-y-4">
      {/* View Mode Selector */}
      <div className="flex items-center gap-2">
        <button
          onClick={() => setViewMode('funnel')}
          className={cn(
            'px-3 py-1.5 text-sm rounded-md transition-colors',
            viewMode === 'funnel'
              ? 'bg-primary text-primary-foreground'
              : 'bg-muted hover:bg-muted/80'
          )}
        >
          Funnel View
        </button>
        <button
          onClick={() => setViewMode('horizontal')}
          className={cn(
            'px-3 py-1.5 text-sm rounded-md transition-colors',
            viewMode === 'horizontal'
              ? 'bg-primary text-primary-foreground'
              : 'bg-muted hover:bg-muted/80'
          )}
        >
          Pipeline View
        </button>
      </div>

      {/* Render selected view */}
      {viewMode === 'funnel' ? (
        <FunnelView {...viewProps} />
      ) : (
        <HorizontalView {...viewProps} />
      )}
    </div>
  )
}
