# Content pages: a page that is read

Companion to `content.md` (the words) and `design-system-handoff.md` §7
(the templates). This one is about the surfaces that are *read* rather than
operated: a blog post, a docs page, a changelog entry, a compare page.

A console screen answers "what is wrong and what do I do". A content page
answers a question somebody typed into a search box, and then has to keep
them reading for four minutes. Different job, same system: paper and ink,
1px borders, no card, no second hue, colour only where it is a state.

The whole look of a body of prose is one class, `.op-prose`. An MDX page
gets it by wrapping the body; the `Article` template applies it for you.
There is one copy of the rules, so the guide, the docs site and the blog
cannot drift apart one renderer at a time.

## 1. When to use Article

- Use `Article` when the page is read top to bottom and the reader's job is
  to finish it: a post, a docs page, a changelog entry, a compare page.
- Use `Detail` when the page is a record the reader came to act on. A record
  with four paragraphs in it is still a record.
- Use `Settings` when the page is a form. A docs page with a form in it is
  two pages.
- Give an `Article` a table of contents as soon as it has more than one `h2`.
  Under that, the rail is furniture.
- Put the reading time in the byline and nowhere else. A reader deciding
  whether to start wants who, when and how long — three facts, one line.
- Cap the measure inside the frame (`--op-measure`, ~68ch), never by
  narrowing the frame. Figures, tables and code panes are allowed past it,
  because they are pictures and not lines to read.

## 2. The picture vocabulary

Four kinds of picture, and no fifth:

| Kind | What it is | Drawn with |
|---|---|---|
| screenshot | the real product, framed and captioned | `ImageFigure`, light and dark |
| diagram | ink, 1px strokes, no fill but the ink steps | inline SVG on the tokens |
| chart | the same primitives the console draws | `TimeChart`, `Breakdown`, `Histogram` |
| live block | a real component, running in the page | the primitive, wrapped in `.op-raw` |

- Frame every screenshot with a 1px ink border and caption it. The caption is
  numbered (`fig. 3 · …`) by `.op-prose`'s counter, so the prose can point at
  it by number.
- Ship every screenshot as a pair, light and dark. A page that ships one is a
  page that is broken in half its states. `ImageFigure` takes `src` and
  `dark` and swaps them with the theme.
- Write the alt as what the picture shows, and the caption as what to notice
  in it. They are two different sentences and both are required.
- Never let a picture carry a fact the text does not. A reader on a screen
  reader, a reader with images off and a reader who skims the prose all have
  to get the same page.
- Keep a chart on the primitives. A picture of a chart goes stale the first
  time a token moves, and "here is the shape" is exactly the claim a stale
  picture gets wrong.
- Wrap a live block in `.op-raw`. It is a component, not prose, and it keeps
  its own type.

Never: hero illustrations, stock photography, drop shadows, rounded corners
on an image, browser-chrome mockups, a picture with no alt, a picture with no
caption, or a screenshot that is the only place a fact appears.

## 3. Code, tables, keys

- Make every code block copyable. Code in a document exists to be run, and a
  reader retyping a command is a reader making a typo. `CodeBlock` says the
  language, says the filename when the code belongs to one, and copies with a
  `CopyAction` that answers on itself.
- Set a code pane on the inset tone, mono at 12px, scrolling sideways. Never
  wrap a command across two lines; never round the corners.
- Write a table as a ledger: an `op-label` header row, 1px rules, tabular
  numerals, numbers to the right. Mark a numeric column `data-align="end"`
  (a `---:` column in markdown does it for you) and the value lands under its
  header.
- Set a key as a key. `kbd` is the same badge `Kbd` draws, so a shortcut in a
  sentence and a shortcut in the console look the same.
- Quote a machine verbatim in mono, and translate nothing it wrote —
  `content.md` §3 applies to a post exactly as it applies to an error.

## 4. Before you ship a content page

- Read it at 390 and at 1440, on paper and on night.
- Check every figure has an alt, a caption and a dark twin.
- Check the table of contents matches the headings, because it is built from
  them and a heading with no text is an entry with no name.
- Check every code block runs as pasted.
- Check nothing in it is the only place a fact appears.
