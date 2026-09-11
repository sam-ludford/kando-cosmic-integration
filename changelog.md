<!--
SPDX-FileCopyrightText: Sam Ludford <samludford76@gmail.com>
SPDX-License-Identifier: CC-BY-4.0
-->

# Changelog

## [Unreleased]

### Added

- Initial release: pointer position and work area via `zwlr_layer_shell_v1`, pointer
  warping via `wp_pointer_warp_v1`, window list / focused window / activation via
  `ext_foreign_toplevel_list_v1` and `zcosmic_toplevel_info_v1`, key simulation via
  `zwp_virtual_keyboard_manager_v1`, and named workspaces via `ext_workspace_v1`.
- Keeps Kando's menu window full-size by maximizing it as soon as it maps.
- `contrib/menus/youtube.json`: a condition-based YouTube remote menu; `kando-menu --trigger ID`.
- `contrib/`: systemd user service, `kando-menu` launcher and AppArmor profile (`make service`, `make apparmor`).
- `setup` wizard and `doctor` checks for first-time configuration.
- CLI for scripting and for Kando menu items: `state`, `workspace`, `keys`, ...
