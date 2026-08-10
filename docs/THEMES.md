# Themes

Themes use semantic roles so layout code never embeds presentation colors.
`classic`, `midnight`, ANSI, and no-color presets are bundled. A custom TOML
file may override any documented role while inheriting the rest from classic.
The Classic palette uses Hacker News cream (`#F7F6F0`) with the Y Combinator
orange (`#FF6600`) for branded surfaces and selections. Orange foreground text
uses the darker `highlight` role (`#C44800`) to remain readable on the cream.

Example:

```toml
name = "paper"

[colors]
background = "#f7f6f0"
foreground = "#202020"
accent = "#ff6600"
accent_fg = "black"
muted = "#6b6458"
highlight = "#c44800"
border = "#b7b7ae"
selected_fg = "black"
selected_bg = "#ff6600"
link = "#1a52a0"
success = "#237a3b"
warning = "#9a6700"
error = "#b42318"
```

`accent` and `accent_fg` style the filled `hnx` brand chip. `highlight` is the
accessible foreground role used by headings, metadata, focused borders, and
popup borders. Overriding `accent` also updates `highlight` for backward
compatibility unless the file explicitly provides a different `highlight`
value.

Unknown roles and invalid colors reject the custom file; startup continues
with `classic` and reports the reason. Setting `NO_COLOR` selects a style-only
mapping that assigns no palette colors: every role resolves to the terminal's
own default foreground and background, leaving only modifiers such as bold and
underline to carry emphasis.
