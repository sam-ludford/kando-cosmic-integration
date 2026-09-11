<!--
SPDX-FileCopyrightText: Sam Ludford <samludford76@gmail.com>
SPDX-License-Identifier: CC-BY-4.0
-->

# COSMIC Integration for Kando

This small D-Bus daemon is required for 🌸 [Kando](https://github.com/kando-menu/kando) on the [COSMIC desktop](https://system76.com/cosmic) (cosmic-comp, Wayland).
Kando's COSMIC backend talks to it over the session bus, in the same way the GNOME backend talks to the [GNOME Shell extension](https://github.com/kando-menu/gnome-shell-integration).

cosmic-comp exposes neither a pointer query, nor the wlroots foreign-toplevel or virtual-pointer protocols, nor the GlobalShortcuts portal, so Electron alone cannot open a pie menu under the cursor. The daemon fills these gaps with the protocols COSMIC does offer:

| Kando needs | Provided through |
| --- | --- |
| Pointer position and work area | a transparent `zwlr_layer_shell_v1` overlay that reports `wl_pointer.enter` |
| Focused window, window list, activation | `ext_foreign_toplevel_list_v1` + `zcosmic_toplevel_info_v1` / `zcosmic_toplevel_manager_v1` |
| Pointer warping | `wp_pointer_warp_v1` |
| Key simulation | `zwp_virtual_keyboard_manager_v1` with the seat's own keymap |
| A full-size, transparent menu window | the daemon maximizes Kando's menu window as soon as it maps (cosmic-comp clamps floating windows to two thirds of the output, and fullscreen gets an opaque backdrop) |

It also gives menu items a way to do COSMIC-specific things that key simulation cannot do (cosmic-comp does not run its own shortcuts for virtual-keyboard input): maximize, minimize, close or pin the focused window, and move it to a **named workspace**.

## ⬇️ Installation

You need a Rust toolchain (`cargo`), `pkg-config` and `libxkbcommon-dev`.

```bash
git clone https://github.com/sam-ludford/kando-cosmic-integration.git
cd kando-cosmic-integration
make install   # builds and installs ~/.local/bin/kando-cosmic-helper
make setup     # interactive wizard: floating rule, shortcut, named workspaces
```

The wizard (`kando-cosmic-helper setup`) checks that cosmic-comp offers everything the helper needs, adds the floating-window rule, creates a COSMIC keyboard shortcut for a menu of your choice, and pre-creates named workspaces for the example menu. `kando-cosmic-helper doctor` repeats the checks any time:

```
✓ XDG_CURRENT_DESKTOP = "COSMIC"
✓ zwlr_layer_shell_v1 (v1+)
✓ zcosmic_toplevel_info_v1 (v2+)
...
✓ floating-window rule in ~/.config/cosmic/com.system76.CosmicSettings.WindowRules/v1/tiling_exception_custom
✓ a COSMIC custom shortcut running kando
```

Kando starts the daemon itself when its COSMIC backend initializes; it looks for `kando-cosmic-helper` in `$KANDO_COSMIC_HELPER`, next to the packaged app, and on `$PATH`.

### Floating-window rule (required)

cosmic-comp auto-tiles every new window unless there is an exception for it, and there is no protocol to opt out. `make setup` (or `make rule`) writes this to `~/.config/cosmic/com.system76.CosmicSettings.WindowRules/v1/tiling_exception_custom` (COSMIC reloads it immediately):

```ron
[
    (
        enabled: true,
        appid: "menu.kando.Kando",
        title: "^Kando Menu$",
    ),
]
```

### Opening menus

COSMIC has no GlobalShortcuts portal, so Kando cannot bind shortcuts itself. Add a custom shortcut in *COSMIC Settings → Input Devices → Keyboard → Keyboard Shortcuts → Custom* running

```bash
kando --menu "Menu Name"
```

Mouse buttons work the same way: divert a button in [Solaar](https://github.com/pwr-Solaar/Solaar) and add a rule that executes the command.

## 🧩 D-Bus interface

Bus name `menu.kando.CosmicIntegration`, object `/menu/kando/CosmicIntegration`, interface `menu.kando.CosmicIntegration1`:

| Method | Signature | Description |
| --- | --- | --- |
| `GetWMInfo` | `() → (ssddiiii)` | window title, app id, pointer x/y, work area x/y/w/h |
| `GetPointer` | `() → (dd)` | pointer position |
| `GetFocusedWindow` | `() → (ss)` | title and app id of the activated window |
| `GetOpenWindows` | `() → a(ss)` | all toplevels |
| `FocusWindow` | `(ss) → b` | activate a window by title and app id |
| `MovePointer` | `(dd) → (dd)` | warp by a delta, returns the new position |
| `SimulateKeys` | `(a(ibi))` | X11 keycode, pressed, delay in ms; same shape as the GNOME extension |

## 🖥️ Command line

The same binary doubles as a CLI, which is what the COSMIC example menu in Kando uses:

```
kando-cosmic-helper setup | doctor
kando-cosmic-helper [daemon] [--exit-with-parent] [--pointer-timeout-ms N]
kando-cosmic-helper pointer | workarea | move <dx> <dy>
kando-cosmic-helper windows | focused | focus <app_id> <title>
kando-cosmic-helper state <app_id>|@focused <title> fullscreen|unfullscreen|maximize|unmaximize|toggle-maximize|minimize|unminimize|toggle-sticky|close
kando-cosmic-helper workspace list | goto <name> | send <name> | take <name> | forget <name>
kando-cosmic-helper keys <x11keycode:down|up[:delay-ms]>...
```

`@focused` selects the window that had focus before Kando's menu opened; Kando's own windows are ignored and the helper waits briefly for focus to return after the menu closes.

### Named workspaces

cosmic-comp (1.7) ignores `create_workspace` and `rename` requests but honours pinning, which gives a workspace a stable id. `workspace send|take|goto <name>` therefore pins the trailing empty workspace the first time a name is used and records `name<TAB>id` in `~/.config/kando-cosmic-helper/workspaces`. COSMIC's own overview keeps showing numbers; the names live in your Kando menu. `workspace forget <name>` unpins it again.

## 🗒️ Changelog

See [changelog.md](./changelog.md).

## 📜 License

MIT, see [LICENSE](./LICENSE).
