# Icons

An icon in this system is a *word the reader does not have to read*. It says
what a thing is, or what a control does. It never says how something is going —
that is the state glyph's job, and the two never share a slot (brand §6, "Icons
say what, glyphs say how"; do not restate the argument, it is made there).

## The set

**lucide-react, and nothing else.** One family, monochrome, no second icon
library, no custom SVGs dropped into a component. The two exceptions are
identity marks, not icons: `GitProviderLogo` and `ProjectMark` (see Banned).

**Stroke width is 1.75**, set once for the whole skin:

```css
.operator.ink svg.lucide { stroke-width: 1.75; }
```

Not lucide's default 2 (too heavy beside Geist at 14px), not the base
`.operator` skin's 1.5 (too faint against ink borders). Never set
`strokeWidth` on an individual icon.

**Sizes**, as the code actually uses them:

| Size | Class | Where |
|---|---|---|
| 16px | `size-4` / `h-4 w-4` | The kind slot before a name: `LedgerRow.icon`, `PickerOption.icon`, `Breakdown` row `icon`, palette `CommandItem`, `PageState`. Always `shrink-0`. |
| 14px | `h-3.5 w-3.5` / `size-3.5` | Inside a label, a button, a badge; inline row actions at the end of the row they act on. The most common size in the sandbox by a wide margin (41 uses). |
| 12px | `h-3 w-3` | Disclosure chevrons only (`viz.tsx` span trees, `Flow` rows). Not a size to reach for. |
| 20px | `h-5 w-5` | Sandbox demo scale-ups only. Not a console size. |

Two sizes are the vocabulary: **16px in rows, 14px in labels and buttons.**
Anything else needs a reason in the PR.

**Monochrome ink only.** An icon is `text-muted-foreground` in a kind slot and
`currentColor` everywhere else. Never `text-destructive` on an icon, never a
brand hue, never a fill.

## The vocabulary

One concept, one icon, forever. This table is the whole allowed set — if a
concept is not here, it does not have an icon yet, and adding one is a PR (see
below). Rendered live at `TokenBlocks` (icon · concept · where used).

### Resources and kinds

| Concept | Icon | Where |
|---|---|---|
| project | `Box` | ledgers, palette, system map |
| service / container | `Container` | agent tool rows, op-components |
| deployment | `Rocket` | deploy ledger, palette, guide |
| database | `Database` | database pages, settings, landing |
| storage / volume | `HardDrive` | database detail, nodes, system map |
| node / compute | `Cpu` | nodes ledger, system map, guide |
| server / instance | `Server` | nodes, observe, settings |
| network / topology | `Network`, `Waypoints` | settings routing, system map |
| domain / region | `Globe` | domains, analytics geography, settings |
| route | `Route` | proxy routes |
| environment / branch | `GitBranch` | env pickers, deploy rows |
| file | `FileText` | agent file rows, palette |
| file being edited | `FilePen` | agent write tool |
| log / terminal | `Terminal` | log panes, agent shell tool |
| log stream | `ScrollText` | console nav |
| code / stack frame | `Code` | error detail |
| agent | `Bot` | agent chat, analytics, landing |
| reasoning step | `Brain` | agent chat |
| task list | `ListChecks`, `ListOrdered` | agent plan blocks |
| user | `User`, `Users`, `UsersRound` | error detail, team settings |
| api key / secret | `Key`, `KeyRound` | settings keys |
| tag / label | `Tag` | analytics dimensions |
| plugin | `Puzzle` | settings plugins |
| layer / stack | `Layers` | console nav |
| folder | `FolderOpen` | console nav |
| archive | `Archive` | system map |
| cloud | `Cloud` | system map |
| video / session replay | `Video` | replay, landing |
| screenshot | `Camera` | email preview |
| error / crash | `Bug` | error tracking |
| build | `Hammer` | build settings |
| schedule / cron | `Timer`, `Hourglass` | scheduled jobs, retention |
| health check | `HeartPulse` | system map |
| metric / gauge | `Gauge` | monitoring, system map |
| chart | `BarChart3` | analytics nav, system map |
| activity | `Activity` | console overview, analytics |
| performance | `Zap` | web vitals, agent speed |
| security | `ShieldCheck` / `ShieldOff` | settings, agent permissions |
| notification | `Bell` / `BellOff` | alerts, error mute |
| announcement | `Megaphone` | analytics campaigns |
| feed | `Rss` | status page |
| email message | `Mail`, `MailOpen`, `MailCheck`, `MailX` | email event kinds |
| inbox / queue | `Inbox` | email, observe, foundations |
| click | `MousePointerClick` | email + error events, system map |
| device: desktop / tablet / phone | `Monitor` / `Tablet` / `Smartphone` | analytics device breakdown |
| browser / discovery | `Compass` | analytics browsers |
| attachment | `Paperclip` | agent composer |
| link | `Link` | linked resources |
| settings | `Settings`, `Cog` | settings pages, `PageState` |
| help | `HelpCircle` | agent chat |

### Actions

| Concept | Icon | Where |
|---|---|---|
| create / add | `Plus` | every "new" action |
| edit | `Pencil` | inline edit |
| delete | `Trash2` | inline destructive |
| remove from a list | `Minus` | env var rows |
| copy | `Copy` / `CopyIcon` (→ `CheckIcon`, `XIcon`) | `CopyButton` |
| retry / rerun | `RotateCcw` | retry actions |
| refresh / reload | `RefreshCw` | manual refresh, `PageState` retry (spins) |
| run / play | `Play` | admin jobs |
| stop | `Square` | agent stop, deploy cancel |
| send | `Send` | composer, test email |
| upload / import | `Upload`, `ArrowUpFromLine` | env import |
| download / export | `Download` | landing, exports |
| share | `Share2` | analytics share |
| search / filter | `Search`, `Filter` | ledger filter, palette |
| open externally | `ExternalLink` | anything leaving the console |
| navigate / go | `ArrowRight`, `ArrowUpRight` | links out of a section |
| swap / compare | `ArrowLeftRight` | comparison |
| promote / upgrade | `ArrowUpCircle` | plan upgrade |
| submit | `ArrowUp` | chat composer |
| expand / collapse | `Maximize2` / `Minimize2` | full-screen panes |
| approve | `Check` | confirmations, selected option |
| dismiss / close | `X` | dialogs, chips |
| more | `MoreHorizontal` | overflow menus |
| disclose | `ChevronRight` / `ChevronDown` / `ChevronUp` / `ChevronsUpDown` | trees, selects, `Picker` |
| columns | `Columns3` | ledger column control |
| menu | `Menu` | mobile nav |
| theme | `Sun` / `Moon` | theme toggle |
| show / hide a secret | `Eye` / `EyeOff` | `SecretValue` |
| feedback | `ThumbsUp` / `ThumbsDown` | agent turn feedback |
| favourite | `Star` | landing |
| warning callout | `TriangleAlert` | `Callout`, email DNS |
| working | `Loader2` (`animate-spin`) | inside a button, never as a page |
| circle / radio dot | `Circle` | radio menu items |

Where two names appear for one concept (`Copy`/`CopyIcon`,
`Terminal`/`TerminalIcon`, `Settings`/`SettingsIcon`, `X`/`XIcon`), that is an
import alias to dodge a local name collision, not a second icon.

## Adding a concept

One PR, three things, no more:

1. **One row in the table above.** Concept in the left column in the same
   voice as its neighbours (a noun for a kind, a verb for an action).
2. **The gallery block** — the icon appears in `TokenBlocks` automatically, as
   it renders from this vocabulary.
3. **Use it.** A concept with no call site is not a concept.

Before adding: read the table. If your concept is a synonym of one already
there ("remove" vs "delete", "reload" vs "refresh"), use the existing icon and
change your word instead. Two icons for one concept is the failure this
document exists to prevent.

## Banned

- **Brand-coloured logos.** Two exceptions, both identity marks and both
  components: `GitProviderLogo` (the provider's own mark beside a repo) and
  `ProjectMark` (a project's favicon, 16px in a row / 24px beside a title,
  monogram fallback in ink). Nothing else may carry a brand hue.
- **Filled icons.** lucide outline only. A filled glyph reads as a state.
- **Emoji.** In UI, in copy, in a `PageState`, in a commit that touches these
  files. Ever.
- **Two icons for one concept**, or one icon for two concepts.
- **Icons as bullets.** A list of checkmarks down the left edge is decoration;
  a `<ul>` is a list.
- **A coloured icon.** Colour is emitted through `Status` only: glyph, word,
  tone. Never `text-destructive` on a `Bug`.
- **A kind icon in the state glyph's slot**, and a state glyph in the kind
  slot. They are two fixed slots and they stay two.
- **`Sparkles`, `Wand2`, `WandSparkles`** and anything else that means "magic".
  AI is `Bot` and `Brain`.
- **A per-icon `strokeWidth`** or `size` prop. Use the class.

---

Rules digest: `RULES.md` §Icons. Reference: `/op-components`, `/v1`, and the
icon table in `TokenBlocks`.
