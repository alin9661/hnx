# Themes

Themes use semantic roles so layout code never embeds presentation colors.
`classic`, `midnight`, ANSI, and no-color presets are bundled. A custom TOML
file may override any documented role while inheriting the rest from classic.

Example:

```toml
name = "paper"

[colors]
background = "#f6f1e1"
foreground = "#202020"
accent = "#ff6600"
muted = "#6b6458"
highlight = "#a44700"
border = "#b7b7ae"
selected_fg = "black"
selected_bg = "#d9c9a3"
link = "#1a52a0"
success = "#237a3b"
warning = "#9a6700"
error = "#b42318"
```

Unknown roles and invalid colors reject the custom file; startup continues
with `classic` and reports the reason. Setting `NO_COLOR` selects a style-only
mapping with no color escape sequences.
