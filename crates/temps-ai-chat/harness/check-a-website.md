---
name: check-a-website
description: Check built pages for what is broken or looks generated, and fix each failure before calling them done
requires_tool: sandbox_exec
contexts: assistant, project
---

# Check a website before calling it done

You cannot see the page, so read the code and run commands. Every question
below must be answered **no**. Fix each failure and check again, then tell the
user the result in one line, for example "Checks: 24 of 24 pass".

## Broken

1. Does the build (`npm run build` or the project's own) fail, or print type
   or lint errors?
2. Does the dev server's log show an error or warning? A Next.js "Issues"
   badge in the preview means it does.
3. Does a CSS variable refer to itself, such as
   `--font-sans: var(--font-sans);`? That is invalid and drops the whole page
   to Times. This must print nothing:
   `grep -rnE --include='*.css' --exclude-dir=node_modules --exclude-dir=.next -- '--([a-z0-9-]+):[[:space:]]*var\(--\1\)' .`
4. Is any font family in the tokens not actually loaded (by `next/font`,
   `@fontsource` or a font link)?
5. Does the page's HTML (fetch it with `curl -s`) lack a real title and
   description, skip a heading level, or have an image without `alt`?

## Looks generated

6. Is the display font Inter, Roboto, Open Sans, Poppins, Lato or the system
   default? (Modern-minimal may use an Inter-class sans on purpose.)
7. Is there a purple-to-blue or cyan-to-magenta gradient, or gradient text,
   anywhere?
8. Does the page run hero, three feature cards, call to action, footer?
9. Is there a row of three equal icon-above-heading cards, a card inside a
   card, or a card with a thick coloured side stripe?
10. Is the hero a full screen with every element centred on one axis?
11. Is any heading italic, or does a heading italicise one word?
12. Does every section carry a small label such as `01 · FEATURES`, or does a
    label sit beside its heading instead of above it?
13. Is the navigation a logo, four or five links and a button on a white bar,
    or the footer four link columns with social icons, when the page does not
    need that?
14. Is any metric, testimonial, customer logo or count invented?
15. Is there a drawn browser bar, phone frame or three-dot window, an emoji
    used as an icon, or more than one icon set?
16. Is there decoration with no meaning in the content, such as floating orbs,
    aurora blobs or a random number?

## Craft

17. Is any colour or font written outside the tokens (a hex value, `rgb()` or
    a font name inside a component)?
18. Do more than three font families appear?
19. Does the accent fill more than about 5% of any screen?
20. Is any spacing off the 4px scale, or is every section spaced the same?
21. Is any paragraph wider than 75 characters or narrower than 45?
22. Does a control lack hover, active, disabled or `:focus-visible` styles,
    or does the page use `transition: all`, a bounce easing, the same hover
    scale on everything, or motion that `prefers-reduced-motion` does not turn
    off?
23. Is body text below 4.5:1 contrast against the background it actually sits
    on, or large text, an icon or a focus ring below 3:1?
24. Can the page scroll sideways at 320px wide, does a button or link label
    wrap onto two lines, or does a grid track holding images use a plain `1fr`?

## Show it

When everything passes, show it with `sandbox_preview` and ask the user what
they think of the direction. Name the genre and shape you chose, so they can
ask for a different one.
