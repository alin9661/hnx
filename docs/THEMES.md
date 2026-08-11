# Themes

Themes use semantic roles so layout code never embeds presentation colors.
`classic`, `midnight`, ANSI, and no-color presets are bundled. A custom TOML
file may override any documented role while inheriting the rest from classic.
The Classic palette uses Hacker News cream (`#F7F6F0`) with exact black
(`#000000`) primary text. Y Combinator orange (`#FF6600`) is reserved for the
masthead, focus, and status accents; selection uses neutral gray (`#E5E4DE`).

Example:

```toml
name = "paper"

[colors]
background = "#f7f6f0"
foreground = "#000000"
accent = "#ff6600"
accent_fg = "black"
muted = "#6b6458"
highlight = "#c44800"
border = "#b7b7ae"
selected_fg = "black"
selected_bg = "#e5e4de"
link = "#1a52a0"
success = "#237a3b"
warning = "#9a6700"
error = "#b42318"
```

`accent` and `accent_fg` style the full-width masthead. `accent` also marks the
focused pane rule and title. `foreground` styles bold primary content,
`muted` styles metadata, `border` styles inactive rules and separators, and
`link` styles underlined URLs. Comment-depth rails cycle through semantic
accent, link, success, warning, and muted roles. Overriding `accent` also
updates `highlight` for backward compatibility unless the file explicitly
provides a different `highlight` value.

Unknown roles and invalid colors reject the custom file; startup continues
with `classic` and reports the reason. Setting `NO_COLOR` selects a style-only
mapping that assigns no palette colors: every role resolves to the terminal's
own default foreground and background, leaving only modifiers such as bold and
underline to carry emphasis.
