# Forms and validation

A form is a page the reader is changing. It owes them three things at every
moment: what each field is, whether what they typed is wrong, and whether the
change is saved. Everything below follows from that.

Companion to `brand-guidelines.md` §6 and `design-system-handoff.md` §6–§7.
Components: `Field`, `FormErrors`, `Settings`, `Callout`, `EchoDialog`,
`SecretValue`, `Picker`. Examples are from the mockups (`/v1?p=settings`,
`/v1?p=email`, `/v1?p=env`).

## Field anatomy

- Give every control a `Field`: label, optional hint, the control, the error line.
- Keep the label visible at all times, at weight 500 (`.op-label`).
- Write the placeholder as an example, never as the name (`ghcr.io/acme`, not "registry URL").
- Put advice in `hint`; it stays put while an error shows, because advice and fault are different things.
- Say when the change takes effect in the hint: "now", "next request", "restart" (Settings does this on every field).
- Render the error under the hint, as glyph + sentence in the destructive tone. That is the only colour a field carries.
- Pass `id`, or use the render-prop form, so `aria-describedby` reaches the hint and the error and `aria-invalid` is set while the error is.
- Keep the row height when there is no hint and no error. A form that jumps as you leave a field is a form that lost your place.

## Validation timing

- Validate a field when it loses focus. Not before.
- Validate the whole form on submit, including the fields never touched.
- Never validate on every keystroke. "Enter a valid URL" while someone types `h` is an insult with a lint rule behind it.
- Re-validate on every keystroke *once a field is already in error*, so the message clears the moment it is fixed.
- Show a format preview live where the shape is the point: the slug under a project name, the record under a domain, the cron sentence under a schedule. A preview is not an error.
- Ask the server on blur when only the server knows (name taken, domain resolves, credentials work), and say what is happening while it waits: "checking DNS…".
- Never let a pending server check block typing in another field.

## Error placement

- Put the message under the field it belongs to, in `Field.error`.
- Write it as a state word plus a sentence that names the resource and the fix: "unreachable · ghcr.io/acme refused the token; check the registry password".
- Never write "invalid", "required" or "error" alone. A word that fits every field explains none of them.
- Add a `FormErrors` Callout at the top of the form when more than one field fails on submit. One field is already marked in place.
- Make every summary entry a button that moves DOM focus to its field, not a highlight.
- Keep the inline message when the summary shows. The summary is a way in, not a second copy of the truth.
- Never raise the summary (`.op-raise`) and never box it. A fault is a Callout; the left rule is the alert.
- Quote the other system verbatim in `Callout.quote` when the fault came from one: the provider's 403, the DNS answer, the build's last line.

## Required and optional

- Mark the exception, not the rule. Most fields required → mark `optional`; most optional → mark the required ones.
- The console's forms are mostly required, so the console marks `optional` and nothing else. There is no asterisk anywhere.
- Never mark both. A form with "required" and "optional" on it has told the reader nothing twice.
- Say what happens if an optional field is left empty, in the hint: "0 = unlimited · cores".

## Disabled and unavailable

- Never disable a control without saying why beside it, in the hint or a Callout: "shm_size is set when the database is created; make a new one to change it".
- Disable only for a reason the reader can read and act on. "Not yet" is a reason; "not for you" is a permission error and says so.
- Do not disable a control because a form is invalid. Let the reader submit and show them what failed — a dead save button explains nothing.
- Onboard, never hide, when a control needs configuration: show it, say what is missing, give an example of what it will do, link the settings page that fixes it (`PageState state="unconfigured"`, or a Callout in a section).
- Keep the save button disabled only while nothing is dirty. That is a fact about the form, not a judgement about the reader.

## Saving

- Give a form one save: the `Settings` sticky bar at the bottom, "unsaved changes" on the left, `save ⌘S` on the right.
- Offer discard beside save the moment the form is dirty, and never anywhere else.
- Show the saving state on the button itself; the bar stays where it is and the form stays readable.
- Confirm the save as the page's verdict — the status line, or a toast that names the object — never both.
- Reset the bar to "no changes" once saved. A form that still says "unsaved changes" after a save is lying.
- Save the whole form, not a field at a time. A per-field save with a page-level bar is two save models on one screen.
- Ask before leaving a dirty form only when the loss is real (typed work, not a toggle flipped back and forth), and ask with `EchoDialog`.
- Let a toggle that takes effect immediately say so in its hint and stay out of the save bar (`/v1?p=email` open tracking is the counter-example: it is dirty state, and it waits for save).

## Long submits

- Keep the reader on the form. A submit never becomes a spinner page.
- Show progress on the button ("saving…", then the step) and lock the fields while it runs.
- Name the steps when there are steps, as `EchoDialog` does: stop containers, remove routes, revoke certificate.
- Say what happened when it fails, in a Callout above the form, with the fields still holding what was typed. Nothing typed is ever thrown away by a failure.
- Never leave a submitted form in a permanent loading state. A submit that cannot finish says so and offers retry.

## Destructive submissions

- Route every destructive or irreversible submit through `EchoDialog`. There is no other confirm dialog.
- Ask for the typed echo only when the loss is irreversible: deleting a domain, destroying a database, revoking every session.
- Confirm a reversible action in ink and say how to undo it: a deploy, a rollback, a route refresh, a cache clear.
- Use red only where the loss is irreversible. A confirmation is not red because it is important.
- Say what is lost and what is kept, with the number: "512 open and 141 click events from the last 30 days; sent emails are kept".
- Put it in the danger zone when it belongs to the resource, not in the middle of a section.

## Secrets

- Render a secret with `SecretValue`: masked, with reveal and copy.
- Never prefill a stored secret into an input. Show that one is set, its age and its last four characters, and offer replace.
- Say what replacing costs before it is replaced ("services using this key fail until they are redeployed").
- Never put a secret in a hint, a placeholder, a toast or a URL.

## Keyboard

- `⌘S` saves, from anywhere on the page, and presses the real button so it shows its real state.
- `⏎` submits a single-field form and nothing else. In a multi-field form it moves nowhere.
- `esc` closes what is open (a Picker, a dialog). It never discards a form, and it never discards anything silently.
- Ignore every page shortcut while an input has focus.
- Give every key a visible badge and every badge a handler; the key is the accelerator for a control the reader can see.
- Move focus with the summary: a `FormErrors` entry focuses its field, it does not scroll to a highlight.
