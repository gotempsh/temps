# AI harness brand assets

These local SVGs come from first-party sources so the harness UI does not
depend on third-party icon packs or runtime network requests.

- `claude-code.svg`: the starburst mark from Anthropic's official Claude Code
  documentation lockup (`code.claude.com`, official Mintlify CDN). The mark's
  path and `#D97757` fill are unchanged; only the view box is cropped to the
  standalone mark.
- `codex.svg`: OpenAI's mark from the official `openai/plugins` repository
  (`plugins/openai-ads-conversions/assets/icon.svg`), used for the Codex
  harness. The surrounding square was removed so the mark has a transparent
  background; the vector path itself is unchanged.
- `opencode.svg`: `opencode-logo-light.svg` from the official downloadable
  OpenCode brand archive at `opencode.ai/brand`, unchanged apart from XML quote
  normalization.

- `cursor.svg`: the app-icon mark from Cursor's official brand page
  (`cursor.com/brand`, `brand-logo-7.svg`). Its two paths and fills are
  unchanged; the view box is cropped to the icon and the page background
  removed.
- `github-copilot.svg`: GitHub's `copilot-24` octicon from the official
  `primer/octicons` repository (MIT). The path is unchanged; a fill that
  follows the color scheme replaces `currentColor`, which an `<img>` cannot
  inherit.
- `windsurf.svg`: `windsurf-black-symbol.svg` from Windsurf's official brand
  page (`windsurf.com/brand`). The path is unchanged; the view box is cropped
  to the symbol and the fill follows the color scheme.

- `vscode.svg`: the Visual Studio Code "stable" icon (`vscode.svg`) from
  Microsoft's official icon archive (`code.visualstudio.com/brand`,
  version 2021-06-21), unchanged. Visual Studio Code and its icon are
  trademarks of Microsoft Corporation; used under its branding guidelines to
  identify a supported integration.
- `zed.svg`: `assets/images/zed_logo.svg` from the official
  `zed-industries/zed` repository. The path is unchanged; a style block makes
  the fill follow the color scheme.

Claude Code, Codex and OpenCode retrieved 2026-09-03; Cursor, GitHub Copilot,
Windsurf, VS Code and Zed retrieved 2026-11-25.
