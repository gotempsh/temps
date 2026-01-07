import { useState } from "react"
import { ChevronRight, ChevronDown, Copy, Check } from "lucide-react"
import { cn } from "@/lib/utils"

interface JsonViewerProps {
  data: unknown
  className?: string
  initialExpanded?: boolean
}

type JsonValue = string | number | boolean | null | JsonObject | JsonArray
interface JsonObject {
  [key: string]: JsonValue
}
type JsonArray = JsonValue[]

function CopyValueButton({ value }: { value: string }) {
  const [copied, setCopied] = useState(false)

  const handleCopy = () => {
    navigator.clipboard.writeText(value)
    setCopied(true)
    setTimeout(() => setCopied(false), 1500)
  }

  return (
    <button
      onClick={handleCopy}
      className="ml-2 p-0.5 hover:bg-muted rounded opacity-0 group-hover:opacity-100 transition-opacity"
    >
      {copied ? (
        <Check className="h-3 w-3 text-success" />
      ) : (
        <Copy className="h-3 w-3 text-muted-foreground" />
      )}
    </button>
  )
}

function JsonValue({ value, depth = 0 }: { value: JsonValue; depth?: number }) {
  const [isExpanded, setIsExpanded] = useState(depth < 2)

  if (value === null) {
    return <span className="text-muted-foreground italic">null</span>
  }

  if (typeof value === "boolean") {
    return <span className="text-purple-500">{value ? "true" : "false"}</span>
  }

  if (typeof value === "number") {
    return <span className="text-blue-500">{value}</span>
  }

  if (typeof value === "string") {
    // Check if it's a URL
    if (value.startsWith("http://") || value.startsWith("https://")) {
      return (
        <span className="text-green-600 group flex items-center">
          <a
            href={value}
            target="_blank"
            rel="noopener noreferrer"
            className="hover:underline"
          >
            "{value}"
          </a>
          <CopyValueButton value={value} />
        </span>
      )
    }
    return (
      <span className="text-green-600 group flex items-center">
        "{value}"
        <CopyValueButton value={value} />
      </span>
    )
  }

  if (Array.isArray(value)) {
    if (value.length === 0) {
      return <span className="text-muted-foreground">[]</span>
    }

    return (
      <span>
        <button
          onClick={() => setIsExpanded(!isExpanded)}
          className="inline-flex items-center hover:bg-muted rounded p-0.5"
        >
          {isExpanded ? (
            <ChevronDown className="h-3 w-3" />
          ) : (
            <ChevronRight className="h-3 w-3" />
          )}
        </button>
        <span className="text-muted-foreground">
          [{!isExpanded && `${value.length} items`}
        </span>
        {isExpanded && (
          <div className="ml-4 border-l border-muted pl-2">
            {value.map((item, index) => (
              <div key={index} className="py-0.5">
                <span className="text-muted-foreground mr-2">{index}:</span>
                <JsonValue value={item} depth={depth + 1} />
              </div>
            ))}
          </div>
        )}
        <span className="text-muted-foreground">{isExpanded && "]"}</span>
      </span>
    )
  }

  if (typeof value === "object") {
    const entries = Object.entries(value)
    if (entries.length === 0) {
      return <span className="text-muted-foreground">{"{}"}</span>
    }

    return (
      <span>
        <button
          onClick={() => setIsExpanded(!isExpanded)}
          className="inline-flex items-center hover:bg-muted rounded p-0.5"
        >
          {isExpanded ? (
            <ChevronDown className="h-3 w-3" />
          ) : (
            <ChevronRight className="h-3 w-3" />
          )}
        </button>
        <span className="text-muted-foreground">
          {"{"}
          {!isExpanded && `${entries.length} keys`}
        </span>
        {isExpanded && (
          <div className="ml-4 border-l border-muted pl-2">
            {entries.map(([key, val]) => (
              <div key={key} className="py-0.5">
                <span className="text-amber-600">"{key}"</span>
                <span className="text-muted-foreground">: </span>
                <JsonValue value={val} depth={depth + 1} />
              </div>
            ))}
          </div>
        )}
        <span className="text-muted-foreground">{isExpanded && "}"}</span>
      </span>
    )
  }

  return <span>{String(value)}</span>
}

export function JsonViewer({ data, className, initialExpanded = true }: JsonViewerProps) {
  return (
    <div
      className={cn(
        "font-mono text-xs p-3 rounded-lg bg-muted/50 overflow-x-auto",
        className
      )}
    >
      <JsonValue value={data as JsonValue} depth={initialExpanded ? 0 : 10} />
    </div>
  )
}
