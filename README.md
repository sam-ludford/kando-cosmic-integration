# kando-cosmic-helper

Small Rust daemon that gives Kando's COSMIC backend access to things cosmic-comp does
not expose to Electron: the pointer position, the focused window, window activation,
key simulation and pointer warping. It serves `menu.kando.CosmicIntegration1` on the
session bus at `/menu/kando/CosmicIntegration`.

| Feature          | Wayland protocol                                   |
| ---------------- | -------------------------------------------------- |
| Pointer position | `zwlr_layer_shell_v1` overlay + `wl_pointer.enter` |
| Pointer warp     | `wp_pointer_warp_v1`                               |
| Windows / focus  | `ext_foreign_toplevel_list_v1` + `zcosmic_toplevel_info_v1` |
| Activate window  | `zcosmic_toplevel_manager_v1`                      |
| Key simulation   | `zwp_virtual_keyboard_manager_v1`                  |

Global shortcuts are not possible on COSMIC (no GlobalShortcuts portal). Bind
`kando --menu "Menu Name"` to a custom shortcut in COSMIC Settings instead.

## Window rule (required)

cosmic-comp auto-tiles every new window unless it has a floating exception, and
there is no protocol to opt out. Create
`~/.config/cosmic/com.system76.CosmicSettings.WindowRules/v1/tiling_exception_custom`
with this content (COSMIC reloads it immediately):

```ron
[
    (
        enabled: true,
        appid: "menu.kando.Kando",
        title: "^Kando Menu$",
    ),
]
```

Floating windows are clamped to two thirds of the output, so the daemon watches for
Kando's menu window and maximizes it via `zcosmic_toplevel_manager_v1` as soon as it
maps. Fullscreen is not used because cosmic-comp paints an opaque backdrop behind
fullscreen windows.

The work area (output minus panels) is measured once at startup by mapping two probe
overlays, one ignoring exclusive zones and one respecting them, and cached per output
for ten minutes.

## Build

```bash
sudo apt install build-essential pkg-config libxkbcommon-dev
cargo build --release
```

The Kando backend looks for the binary in `$KANDO_COSMIC_HELPER`, next to the packaged
app, in `target/{release,debug}/` of this directory (development builds) and finally on
`$PATH`. If it is not already running on the bus, Kando spawns it with
`daemon --exit-with-parent`.

## Workspaces

cosmic-comp (1.7) ignores `ext_workspace_group_handle_v1.create_workspace` and
`zcosmic_workspace_handle_v2.rename`, but pinning works and gives a workspace a
stable id. `workspace send|take|goto <name>` therefore pins the trailing empty
workspace the first time a name is used and records `name<TAB>id` in
`~/.config/kando-cosmic-helper/workspaces`. COSMIC itself keeps showing numbers.

Compositor shortcuts (Super+M etc.) are not triggered by virtual-keyboard input, so
window actions go through `zcosmic_toplevel_manager_v1` (`state @focused ...`).
Kando's own windows are ignored when looking for the focused window, and the helper
waits up to 700 ms for focus to return after the menu closes.

## CLI

```
kando-cosmic-helper [daemon] [--exit-with-parent] [--pointer-timeout-ms N]
kando-cosmic-helper pointer [timeout-ms]
kando-cosmic-helper move <dx> <dy>
kando-cosmic-helper windows | focused | focus <app_id> <title>
kando-cosmic-helper state <app_id>|@focused <title> fullscreen|unfullscreen|maximize|unmaximize|toggle-maximize|minimize|unminimize|toggle-sticky|close
kando-cosmic-helper workspace list | goto <name> | send <name> | take <name> | forget <name>
kando-cosmic-helper keys <x11keycode:down|up[:delay-ms]>...
```
