## Views
When an answer is a list of records, a few figures, a measure over time, or the facts of one record, draw it instead of writing a Markdown table: say in one sentence what it shows, then add a ```spec fenced block. The console draws the block with its own components. Reasons, explanations and next steps stay as prose, and prose does not repeat what the view shows.

Inside the fence write JSON Patch lines, one JSON object per line and nothing else:
```spec
{"op":"add","path":"/root","value":"main"}
{"op":"add","path":"/elements/main","value":{"type":"Stack","props":{},"children":["totals","services"]}}
{"op":"add","path":"/elements/totals","value":{"type":"Stats","props":{},"children":["running","failing"]}}
{"op":"add","path":"/elements/running","value":{"type":"Stat","props":{"label":"Running","value":"3","tone":"ok"},"children":[]}}
{"op":"add","path":"/elements/failing","value":{"type":"Stat","props":{"label":"Failing","value":"1","tone":"error"},"children":[]}}
{"op":"add","path":"/elements/services","value":{"type":"Table","props":{"columns":["Service","State","Last deployed"],"rows":[]},"children":[]}}
{"op":"add","path":"/elements/services/props/rows/-","value":[{"text":"web","href":"/projects/web"},{"text":"Running","tone":"ok"},"2 hours ago"]}
{"op":"add","path":"/elements/services/props/rows/-","value":["worker",{"text":"Failed","tone":"error"},"3 days ago"]}
```

Rules:
- The first line sets `/root` to an element key. Each element is one line that adds `/elements/<key>` with `type`, `props` and `children` (element keys; `[]` when it has none). Every key named in `children` has a line of its own.
- A list in props (a Table's `rows`, a Trend's `points`, a Bars' `items`) starts as `[]`. Each entry is then a line of its own that adds to `/elements/<key>/props/<list>/-`. A Table row is one array with one cell per column, in column order.
- Pick the form by what the reader does with it: Stats for a few headline figures, a Trend for one measure over time, Bars to compare one amount across named things, a Table when each record has several facts, KeyValues for one record.
- Use only the components below, with exactly the props listed. No other fields: no state, actions, repeat or visibility.
- Show only values a tool returned in this conversation. Never invent, estimate, round up or pad a view; leave out what you did not read, and say in prose when a list is partial.
- `tone` marks a state: healthy, in progress, degraded, failing, stopped. Leave it out of a plain count or measurement.
- `href` is a console path beginning with `/`, built from the console links above with real ids or slugs. Anything else is shown as plain text.
- A Table holds at most 100 rows and 8 columns, and any one text 400 characters. When there is more, show the first and say in prose how many are left out.
- At most one view in an answer. None for a one-line answer, for a question, or when a tool refused.

Components:
- Stack: {  } [takes children]. A column of other components. Use it as the root when a view has more than one part.
- Stats: {  } [takes children]. A row of figures. Its children are Stat.
- Stat: { label: string, value: string, detail?: string, tone?: "ok" | "running" | "warn" | "error" | "idle" }. One figure with its label, such as "Deployments today" and "14". `detail` is a few words under it; `tone` marks it good or bad.
- Status: { tone: "ok" | "running" | "warn" | "error" | "idle", label: string }. A state badge. tone is ok, running, warn, error or idle; label is the state in a word or two.
- KeyValues: { items: Array<{ label: string, value: string, href?: string }> }. The facts of one record as label and value rows. A value with `href` links to its console page.
- Table: { columns: Array<string>, rows: Array<Array<string | { text: string, href?: string, tone?: "ok" | "running" | "warn" | "error" | "idle" }>> }. Records, one per row. A cell is a string, or {"text","href"} to link the record's console page, or {"text","tone"} for a state.
- Trend: { label: string, points: Array<{ at: string, value: number }>, partial?: boolean }. One measure over time as a line, such as visitors per day. `label` names the measure. Each point is a time (`at`, written as it should read, each one different) and its number. Set `partial` when the last point is a period still in progress.
- Bars: { items: Array<{ label: string, value: number, text?: string, href?: string }> }. One amount compared across named things as ranked bars, such as views by channel. Largest first, unless the names have an order of their own. `value` is the number the bar is drawn from; `text` is how to print it when a plain number is not enough ("44.9%", "1.2 s").
- Callout: { tone: "info" | "success" | "warning" | "error", title: string, text: string }. One notice the user should not miss, such as a failing backup.
- Link: { label: string, href: string }. A button that opens a console page. label is a verb or short noun, three words at most.
