# SlintLu

Build complete Wayland desktop shells — bars, docks, launchers, notifications, wallpapers,
lock screens, overlays — in the [Slint](https://slint.dev) language. Similar in spirit to
[Quickshell](https://quickshell.outfoxxed.me/), with Slint as the UI language and Rust at the
core.

> **Status:** early development. The Wayland platform and the `slintlu` runtime work; system
> services are being added (see [Roadmap](#roadmap)).

## Features

- **Write the shell in Slint, run it with `slintlu`.** Files are compiled at runtime and
  reloaded on save; a broken edit keeps the running shell and prints diagnostics.
- **Native Wayland** through smithay-client-toolkit, no winit:
  - `wlr-layer-shell` panels with anchors, margins, exclusive zones, keyboard modes, per-screen
    instances and show/hide for popups and drawers; `background-blur` asks the compositor to
    blur what is behind a panel (`ext-background-effect-v1`, where the compositor supports it);
  - `xdg-shell` windows for settings dialogs and the like;
  - multi-monitor with hot-plug, fractional scaling (`fractional-scale-v1` + viewporter),
    clipboard and primary selection, `cursor-shape-v1`, key repeat, virtual keyboards.
- **Three renderers with automatic fallback**

  | `--renderer` / `SLINTLU_RENDERER` | Implementation | Graphics API |
  |---|---|---|
  | `vulkan` | FemtoVG on wgpu | Vulkan |
  | `opengl` | FemtoVG | OpenGL ES 2/3 via EGL (`libEGL` loaded at runtime) |
  | `software` | Slint software renderer | `wl_shm`, no GPU required |
  | `auto` (default) | first that works: Vulkan → OpenGL → Software | |

- **Compatibility:** every compositor implementing layer-shell (niri, Hyprland, Sway, river,
  KDE Plasma, Wayfire, labwc, …) shows shell panels; others (e.g. GNOME) fall back to normal
  windows.
- **Also a Rust library:** use the platform directly with compiled Slint (`slint::slint!`) —
  see [`examples/hello_bar.rs`](examples/hello_bar.rs).

## Quick start

```bash
cargo install --path .
slintlu init        # writes a starter bar, launcher, lock screen and tray menu to ~/.config/slintlu
slintlu             # runs it; edit the files and watch them reload
```

With Nix:

```bash
nix run github:LunaDashWM/SlintLu -- init
nix run github:LunaDashWM/SlintLu
```

or in Home Manager (`inputs.slintlu.url = "github:LunaDashWM/SlintLu";`):

```nix
imports = [ inputs.slintlu.homeManagerModules.default ];
programs.slintlu = {
  enable = true;
  configDir = ./shell;          # linked to ~/.config/slintlu
  theme.dark = "tokyo-night";   # written to theme.json
};
```

`nix develop` provides the toolchain plus sway, grim and wtype for the headless tests.

A minimal panel:

```slint
import { PanelWindow, Shell } from "@slintlu";

export component Bar inherits PanelWindow {
    anchor-top: true;
    anchor-left: true;
    anchor-right: true;
    preferred-height: 32px;
    background: #1e1e2e;
    Text { text: "Hello from " + Shell.screen.name; color: white; }
}
```

Every component, global and struct is listed in the [API reference](docs/API.md).

Run `slintlu library-path` and add the printed directory as the `slintlu` library path in your
editor's Slint extension to get completion for `@slintlu`.

## Running at login

Start `slintlu` from your compositor (`spawn-at-startup "slintlu"` in niri,
`exec-once = slintlu` in Hyprland, `exec slintlu` in Sway) or as a systemd user service with
[`dist/slintlu.service`](dist/slintlu.service). `SIGTERM` quits cleanly and `SIGHUP` reloads.

## Command line

| Command | Purpose |
|---|---|
| `slintlu [-c PATH] [-r RENDERER] [--no-reload]` | run a shell (default config: `~/.config/slintlu/shell.slint`) |
| `slintlu check [PATH]` | compile a configuration and print diagnostics |
| `slintlu init [--dir DIR] [--force]` | create a starter configuration |
| `slintlu outputs` | list monitors |
| `slintlu info` | compatibility report: protocols, renderers, desktop services |
| `slintlu library-path` | print where the `@slintlu` library lives |
| `slintlu msg …` | control the running shell: `reload`, `quit`, `get`, `set`, `toggle`, `call` ([IPC](docs/IPC.md)) |

## Project layout

```
src/
├── core/         Slint platform, event loop, shared Wayland context
├── input/        seat, keyboard (xkb → Slint keys), pointer and scrolling
├── output/       monitors and hot-plug
├── render/       software / OpenGL (EGL) / Vulkan (wgpu) renderers and fallback
├── shell/        layer-shell and xdg-shell roles
├── window/       the Slint window adapter: lifecycle and frame pacing
├── compositors/  compositor detection; LunaDash, niri, Hyprland and Sway IPC
├── files/        directory listings and cached thumbnails
├── store/        persistent key–value settings
├── debug/        diagnostics for `slintlu info`
├── ipc/          the shell's control socket: protocol and client
├── dbus/         notifications, MPRIS, NetworkManager, logind, portal, BlueZ, tray
├── i18n/         .po parsing, .mo writing, locale detection
├── icons/        freedesktop icon theme lookup
├── idle/         ext-idle-notify and idle inhibition
├── clipboard/    copy / paste, primary selection, clipboard history
├── launcher/     desktop entries, search, launch history, launching
├── security/     session lock, PAM authentication, polkit agent
├── audio/        volume and devices (PipeWire / PulseAudio via pactl)
├── system/       CPU, memory, disks, temperatures, batteries, network, host
├── theme/        palettes, theme.json, pywal
├── time/         clock formatting
├── util/         human readable sizes, rates and durations, file:// URIs
├── ui/           the `@slintlu` Slint library
├── scripting/    interpreter bridge: compiling, instantiating, globals   (runtime)
├── config/       configuration paths and hot reload                      (runtime)
└── cli/          the `slintlu` command line                              (runtime)
res/              shared assets shipped with the library (icons for `Icon`, see res/README.md)
examples/         starter configuration and Rust examples
scripts/          headless sway integration test, API docs and icon generators
nix/              Nix package and Home Manager module (see flake.nix)
```

## Roadmap

- [x] Wayland platform: layer-shell, xdg-shell, outputs, input, three renderers
- [x] Runtime: interpreter, per-screen panels, hot reload, CLI
- [x] Services: clock, system stats, battery, network traffic, audio, workspaces and windows (niri / Hyprland / Sway)
- [x] IPC: control a running shell (`slintlu msg …`) from keybindings and scripts — see [docs/IPC.md](docs/IPC.md)
- [x] D-Bus: notification daemon, media players (MPRIS), NetworkManager, power (logind), dark mode and accent colour (portal)
- [x] i18n: `@tr()` with `.po` files compiled at runtime and live language switching — see [docs/I18N.md](docs/I18N.md)
- [x] Applications and icons: desktop entries, fuzzy search ranked by usage, launching (terminal apps, actions), freedesktop icon themes
- [x] Lock screen (ext-session-lock, PAM, logind) and polkit authentication agent — see [docs/LOCKSCREEN.md](docs/LOCKSCREEN.md)
- [x] Clipboard and primary selection, idle detection / inhibition / auto-lock, fractional scaling
- [x] Screen brightness (sysfs + logind, no root) and Bluetooth (BlueZ, never auto-started)
- [x] Theming: built-in palettes, `theme.json`, pywal, desktop dark mode and accent — see [docs/THEMING.md](docs/THEMING.md)
- [x] System tray: StatusNotifierItem host (and watcher when none runs) with dbusmenu menus
- [x] Persistent settings (`Store`), files and thumbnails (`Files`), calendar grids, clipboard history (wlr-data-control)
- [x] LunaDash compositor support (workspaces and windows over its control socket)

## Development

Requires Rust ≥ 1.92, [`just`](https://github.com/casey/just), and the `wayland`,
`libxkbcommon`, `fontconfig` and `pam` development packages.

```bash
just           # list recipes
just test      # unit and integration tests, no compositor needed
just run       # run the starter shell in the current session
just ci        # all checks and tests; CI itself only builds, so run this before pushing
```

See [docs/TESTING.md](docs/TESTING.md) for all ways to test and
[CONTRIBUTING.md](CONTRIBUTING.md) for the pull request process.

## License

GPL-3.0-only (compatible with Slint's GPL licensing option).
