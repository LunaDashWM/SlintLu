# Theming

Style your shell with the roles of the `Theme` global instead of fixed colours:

```slint
import { Theme } from "@slintlu";

Rectangle {
    background: Theme.surface;
    border-radius: Theme.radius;
    Text { text: "Hello"; color: Theme.text; font-family: Theme.font-family; font-size: Theme.font-size; }
}
```

Roles: `background`, `surface`, `surface-alt`, `border`, `text`, `text-muted`, `accent`, `red`,
`green`, `yellow`, `blue`, `magenta`, `cyan`, plus `dark`, `name`, `font-family`, `font-size`,
`radius`, `high-contrast`.

By default the theme follows the desktop's light / dark preference and accent colour (from the
XDG desktop portal, e.g. `gsettings set org.gnome.desktop.interface color-scheme prefer-dark`).
`font-family` is `theme.json`'s `font-family` when it sets one, else the desktop's interface font
(`org.gnome.desktop.interface font-name` through the portal, e.g. "Adwaita Sans 11" gives
"Adwaita Sans"), else empty for Slint's default font. It follows changes of the desktop setting.

## theme.json

Put a `theme.json` next to `shell.slint`; it is re-read on every reload:

```json
{
  "dark": "tokyo-night",
  "light": "catppuccin-latte",
  "mode": "auto",
  "system-accent": true,
  "pywal": false,
  "colors": { "accent": "#ff79c6" },
  "font-family": "Inter",
  "font-size": 13,
  "radius": 10
}
```

| Key | Default | Meaning |
|---|---|---|
| `dark` / `light` | `catppuccin-mocha` / `catppuccin-latte` | palette per mode |
| `mode` | `auto` | `auto` follows the desktop, or force `dark` / `light` |
| `system-accent` | `true` | use the desktop's accent colour when it has one |
| `pywal` | `false` | use pywal / wallust colours from `~/.cache/wal/colors.json` |
| `colors` | `{}` | override any role |
| `palettes` | `{}` | define your own palettes by name (all roles plus `dark`), usable as `dark` / `light` |

Built-in palettes: `catppuccin-mocha`, `catppuccin-latte`, `nord`, `gruvbox-dark`,
`gruvbox-light`, `tokyo-night` (listed in `Theme.palettes`).

At runtime: `Theme.set-mode("light")`, `Theme.set-palette("nord")`, or from scripts
`slintlu msg call Theme.set-palette nord`.
