// SPDX-FileCopyrightText: Sam Ludford <samludford76@gmail.com>
// SPDX-License-Identifier: MIT

//! `doctor` (checks) and `setup` (interactive wizard) for first-time configuration.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};

use crate::pointer::Error;

#[derive(Default)]
struct Globals(Vec<(String, u32)>);

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            interface, version, ..
        } = event
        {
            state.0.push((interface, version));
        }
    }
}

fn globals() -> Result<Vec<(String, u32)>, Error> {
    let conn = Connection::connect_to_env().map_err(|e| Error::Connect(e.to_string()))?;
    let mut queue = conn.new_event_queue::<Globals>();
    let _registry = conn.display().get_registry(&queue.handle(), ());
    let mut state = Globals::default();
    queue
        .roundtrip(&mut state)
        .map_err(|e| Error::Wayland(e.to_string()))?;
    Ok(state.0)
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default()
}

fn rule_path() -> PathBuf {
    config_home().join("cosmic/com.system76.CosmicSettings.WindowRules/v1/tiling_exception_custom")
}

fn shortcuts_path() -> PathBuf {
    config_home().join("cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom")
}

const RULE_ENTRY: &str = r#"    (
        enabled: true,
        appid: "menu.kando.Kando",
        title: "^Kando Menu$",
    ),
"#;

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

fn rule_present() -> bool {
    std::fs::read_to_string(rule_path())
        .map(|s| s.contains("menu.kando.Kando"))
        .unwrap_or(false)
}

fn shortcut_present() -> bool {
    std::fs::read_to_string(shortcuts_path())
        .map(|s| s.contains("kando"))
        .unwrap_or(false)
}

/// Insert `entry` before the closing bracket of a RON list/map file, creating the file
/// with `open`/`close` delimiters if it does not exist.
fn insert_ron_entry(path: &PathBuf, entry: &str, open: char, close: char) -> Result<(), Error> {
    let io = |e: std::io::Error| Error::Wayland(format!("{}: {e}", path.display()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let body = match existing.trim_end().rfind(close) {
        Some(i) if existing.trim_start().starts_with(open) => {
            let mut head = existing[..i].trim_end().to_string();
            if !head.ends_with(open) && !head.ends_with(',') {
                head.push(',');
            }
            format!("{head}\n{entry}{close}\n")
        }
        _ => format!("{open}\n{entry}{close}\n"),
    };
    std::fs::write(path, body).map_err(io)
}

pub fn write_rule() -> Result<(), Error> {
    insert_ron_entry(&rule_path(), RULE_ENTRY, '[', ']')
}

/// `combo` like "Ctrl+Space" or "Super+Shift+m"; COSMIC wants xkb key names.
pub fn write_shortcut(combo: &str, menu: &str) -> Result<(), Error> {
    let mut mods = Vec::new();
    let mut key = String::new();
    for part in combo.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.push("Ctrl"),
            "alt" => mods.push("Alt"),
            "shift" => mods.push("Shift"),
            "super" | "meta" | "win" | "cmd" => mods.push("Super"),
            k => {
                key = if k.len() == 1
                    || k.starts_with('f') && k[1..].parse::<u8>().is_ok() && k.len() > 1
                {
                    if k.len() == 1 {
                        k.to_string()
                    } else {
                        k.to_uppercase()
                    }
                } else {
                    k.to_string()
                }
            }
        }
    }
    if key.is_empty() {
        return Err(Error::Wayland(format!("no key in shortcut {combo:?}")));
    }
    let mods_ron = mods
        .iter()
        .map(|m| format!("            {m},\n"))
        .collect::<String>();
    let entry = format!(
        "    (\n        modifiers: [\n{mods_ron}        ],\n        key: \"{key}\",\n        description: Some(\"Kando: {menu}\"),\n    ): Spawn(\"kando --menu \\\"{menu}\\\"\"),\n"
    );
    insert_ron_entry(&shortcuts_path(), &entry, '{', '}')
}

struct Check {
    ok: bool,
    label: String,
    hint: &'static str,
}

fn checks() -> Vec<Check> {
    let mut out = Vec::new();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    out.push(Check {
        ok: desktop.eq_ignore_ascii_case("cosmic"),
        label: format!("XDG_CURRENT_DESKTOP = {desktop:?}"),
        hint: "Kando selects the COSMIC backend only when this is \"COSMIC\".",
    });
    match globals() {
        Ok(g) => {
            let has = |name: &str, min: u32| g.iter().any(|(n, v)| n == name && *v >= min);
            for (name, min, hint) in [
                ("zwlr_layer_shell_v1", 1, "needed for the pointer position"),
                (
                    "ext_foreign_toplevel_list_v1",
                    1,
                    "needed for the window list",
                ),
                (
                    "zcosmic_toplevel_info_v1",
                    2,
                    "needed for the focused window",
                ),
                (
                    "zcosmic_toplevel_manager_v1",
                    4,
                    "needed for window actions and workspaces",
                ),
                (
                    "zwp_virtual_keyboard_manager_v1",
                    1,
                    "needed for key simulation",
                ),
                ("ext_workspace_manager_v1", 1, "needed for named workspaces"),
                (
                    "zcosmic_workspace_manager_v2",
                    2,
                    "needed for pinning workspaces",
                ),
                ("wp_pointer_warp_v1", 1, "optional: pointer warping"),
            ] {
                out.push(Check {
                    ok: has(name, min),
                    label: format!("{name} (v{min}+)"),
                    hint,
                });
            }
        }
        Err(e) => out.push(Check {
            ok: false,
            label: format!("Wayland: {e}"),
            hint: "Run this inside the COSMIC session.",
        }),
    }
    out.push(Check {
        ok: on_path("kando"),
        label: "kando on $PATH".into(),
        hint: "Install Kando (deb, AppImage, ...); the Flatpak has no COSMIC backend yet.",
    });
    out.push(Check {
        ok: on_path("kando-cosmic-helper"),
        label: "kando-cosmic-helper on $PATH".into(),
        hint: "Run `make install`.",
    });
    out.push(Check {
        ok: rule_present(),
        label: format!("floating-window rule in {}", rule_path().display()),
        hint: "Without it cosmic-comp tiles the menu window. `setup` or `make rule` adds it.",
    });
    out.push(Check {
        ok: shortcut_present(),
        label: "a COSMIC custom shortcut running kando".into(),
        hint: "COSMIC has no GlobalShortcuts portal; `setup` adds one.",
    });
    out
}

pub fn doctor() -> bool {
    let checks = checks();
    let mut all_ok = true;
    for c in &checks {
        println!("{} {}", if c.ok { "✓" } else { "✗" }, c.label);
        if !c.ok {
            println!("    {}", c.hint);
            all_ok = all_ok && c.hint.starts_with("optional");
        }
    }
    all_ok
}

/// Prompt on stdin. An empty line takes the default; a closed stdin aborts the wizard
/// instead of silently accepting every default.
fn ask(prompt: &str, default: &str) -> Result<String, Error> {
    print!("{prompt} [{default}]: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let n = std::io::stdin().lock().read_line(&mut line).unwrap_or(0);
    if n == 0 {
        println!();
        return Err(Error::Wayland("stdin closed; setup aborted".into()));
    }
    let t = line.trim();
    Ok(if t.is_empty() {
        default.to_string()
    } else {
        t.to_string()
    })
}

fn confirm(prompt: &str, default_yes: bool) -> Result<bool, Error> {
    let a = ask(prompt, if default_yes { "Y/n" } else { "y/N" })?;
    Ok(match a.to_ascii_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default_yes,
    })
}

/// Interactive first-time setup.
pub fn setup() -> Result<(), Error> {
    println!("Kando COSMIC integration setup\n");
    doctor();
    println!();

    if !rule_present() && confirm("Add the floating-window rule for Kando's menu?", true)? {
        write_rule()?;
        println!("  wrote {}", rule_path().display());
    }

    if confirm(
        "Add a COSMIC keyboard shortcut that opens a Kando menu?",
        true,
    )? {
        let menu = ask("  Menu name", "COSMIC Menu")?;
        let combo = ask("  Shortcut (e.g. Ctrl+Space, Super+m)", "Ctrl+Space")?;
        write_shortcut(&combo, &menu)?;
        println!(
            "  wrote {} (COSMIC reloads it immediately)",
            shortcuts_path().display()
        );
    }

    if confirm("Create named workspaces for the example menu?", true)? {
        let names = ask(
            "  Names, comma separated",
            "Coding, Browsing, Files, Chat, Media",
        )?;
        for name in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            match crate::workspace::goto_workspace_no_switch(name) {
                Ok(()) => println!("  workspace {name:?} ready"),
                Err(e) => println!("  workspace {name:?}: {e}"),
            }
        }
    }

    println!("\nDone. Next:");
    println!("  - (re)start Kando; it spawns kando-cosmic-helper itself");
    println!("  - press the shortcut, or run: kando --menu \"COSMIC Menu\"");
    println!("  - mouse buttons: divert one in Solaar and add a rule executing the same command");
    println!("  - re-check any time with: kando-cosmic-helper doctor");
    Ok(())
}
