# Notifications: which surface says it

Five surfaces carry messages, and every message belongs to exactly one of
them. Pick by asking what kind of message it is, not by what is convenient to
call from the handler.

Companion to `brand-guidelines.md` §6 and `design-system-handoff.md` §6.
Components: `StatusLine` / `AttentionHost`, `Callout`, the sandbox's
`notify()` (sonner), `EchoDialog`.

## The decision table

| Trigger | Surface | Persistence | Severity allowed | Carries an action? | Console example |
| --- | --- | --- | --- | --- | --- |
| A verdict about the page the reader is on | `StatusLine` (portals into the header's attention slot; inline outside a shell) | While the page is open; recomputed on every load | `ok` `warn` `error` `idle` `sampled` | One `Phrase`, on the thing to act on | `× billing-worker is failing health checks.` with `+1 warning` |
| A fault or a warning that belongs to one thing on the page | `Callout`, inline directly above what it applies to | Until it is fixed; it is state, not an event | `error` `warn`; `ok` only when it carries proof | One action button, right of the text | `× the git connection to github/acme expired` above the deployments ledger, with `reconnect` |
| The result of an action the reader just took | Toast (`notify()`) | ~6s, auto-dismiss, stackable | `ok` `warn` `err` | An undo, and never the only one | `ok · api-gateway deploying · dep_93a` |
| Something that happened while the reader was elsewhere | The bell / `AttentionHost`, counted by state | Until read | `error` and `warn` counted; `ok` is the quiet glyph | Each entry links to its page | `× 2 ◐ 1` in the header, opening to one line per item |
| A decision that must be made before anything else happens | `EchoDialog` | Until answered | Red only when the loss is irreversible | The action is the whole point | `Remove mail.acme.sh` — typed echo, then three steps |

## Rules

- One surface per message. Never a toast and a Callout for the same event.
- A fault that persists is a Callout. A toast that has to be read is a bug: it will be gone before the reader looks up.
- Write a toast as state · headline · fact: `ok`, `settings saved`, `applies to emails sent from now on`.
- Keep the headline to six words or fewer and name the object in it: `api-gateway deploying · dep_93a`, never `Success!`.
- Never say "success", "done" or "error" alone. A message that names nothing proves nothing.
- Use only `ok` / `warn` / `error` as severity words, with their glyphs (● ◐ ×), per the status vocabulary. No "info", no "notice", no "critical".
- Never make the toast the only place an undo exists. The undo lives on the object too; the toast is a shortcut to it.
- Count in the bell by state, with the same glyphs the rest of the console uses. `× 2 ◐ 1` is a sentence; a badge is not.
- Unread is a count. A red dot with no number tells the reader to go looking, which is the one thing a notification exists to prevent.
- Show one quiet glyph and no number when nothing needs attention. Zero is not an alarm.
- Never move the layout to say something. No banner pushing the page down, no row appearing above the header. The one exception is the `Settings` sticky save bar, which is the form's own state and belongs to the form.
- Never toast on page load. What was already true when the page opened is a verdict or a Callout.
- Never toast in a loop. Ten failed rows are one Callout with a count, not ten toasts.
- Announce with the right politeness: `role="alert"` for an error Callout, `role="status"` for everything else. `StatusLine` and `Callout` already do this.

## The sandbox's `notify()`

The design-system sandbox has no toast component of its own and the package
ships none: toasts are `sonner`, mounted once in `main.tsx` (`<Toaster expand />`),
and the console wraps it in one hook so every caller writes the same shape.

```ts
const notify = useNotify()
notify(level, msg, detail?, undo?)
//     │      │     │        └ optional; a cheap change is confirmed by its
//     │      │     │          consequence, and the undo rides the toast
//     │      │     └ the fact: an id, a count, a scope ("dep_93a", "41 routes")
//     │      └ the headline: ≤ 6 words, names the object
//     └ 'ok' | 'warn' | 'err' — the status vocabulary, nothing else
```

- Pass the skin class to the toast portal, like every other portalled surface: it renders outside the `.operator` root.
- A `notify` without an `undo` is a plain confirmation; adding one turns the action into "do it, then offer the way back", which is how a reversible change is confirmed.
- Real toasts (`ConsoleV1.tsx`): `notify('ok', 'settings saved', 'applies to emails sent from now on')`, `notify('ok', 'test email sent', 'mail.acme.sh → maya@acme.sh')`, `notify('warn', 'tracking disabled', '653 events deleted')`, `notify('ok', 'rolled back to dep_90e', undefined, restore)`.
