// SPDX-FileCopyrightText: Sam Ludford <samludford76@gmail.com>
// SPDX-License-Identifier: MIT

//! Key simulation via `zwp_virtual_keyboard_manager_v1`.
//!
//! The seat's real keymap is forwarded to the virtual keyboard and mirrored in an
//! xkb state so modifier masks can be reported correctly.

use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_keyboard, wl_registry, wl_seat};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};
use xkbcommon::xkb;

pub use crate::pointer::Error;

/// One key transition. `keycode` is an X11 keycode (evdev code + 8), which is what
/// Kando's key-code table provides for Linux.
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub keycode: i32,
    pub down: bool,
    pub delay_ms: i32,
}

#[derive(Default)]
struct State {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>,
    keymap: Option<(OwnedFd, u32)>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(7), qh, ()));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.manager = Some(registry.bind(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format, fd, size } = event {
            if format == WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) {
                state.keymap = Some((fd, size));
            }
        }
    }
}

delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1);

pub fn simulate_keys(keys: &[KeyEvent]) -> Result<(), Error> {
    if let Some(bad) = keys.iter().find(|k| k.keycode < 8) {
        return Err(Error::Wayland(format!(
            "invalid X11 keycode {}",
            bad.keycode
        )));
    }

    let conn = Connection::connect_to_env().map_err(|e| Error::Connect(e.to_string()))?;
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut state = State::default();
    let rt = |q: &mut wayland_client::EventQueue<State>, s: &mut State| {
        q.roundtrip(s)
            .map(|_| ())
            .map_err(|e| Error::Wayland(e.to_string()))
    };
    rt(&mut queue, &mut state)?;

    let seat = state.seat.clone().ok_or(Error::MissingGlobal("wl_seat"))?;
    let manager = state
        .manager
        .clone()
        .ok_or(Error::MissingGlobal("zwp_virtual_keyboard_manager_v1"))?;

    // Fetch the real keymap from the seat.
    let keyboard = seat.get_keyboard(&qh, ());
    rt(&mut queue, &mut state)?;
    keyboard.release();
    let (fd, size) = state
        .keymap
        .take()
        .ok_or_else(|| Error::Wayland("seat did not provide an xkb keymap".into()))?;

    let vk = manager.create_virtual_keyboard(&seat, &qh, ());
    vk.keymap(wl_keyboard::KeymapFormat::XkbV1 as u32, fd.as_fd(), size);

    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = unsafe {
        xkb::Keymap::new_from_fd(
            &ctx,
            fd,
            size as usize,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
    }
    .map_err(|e| Error::Wayland(format!("cannot map keymap: {e}")))?
    .ok_or_else(|| Error::Wayland("cannot compile keymap".into()))?;
    let mut xkb_state = xkb::State::new(&keymap);

    let start = Instant::now();
    let result = (|| {
        for key in keys {
            if key.delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(key.delay_ms as u64));
            }
            let direction = if key.down {
                xkb::KeyDirection::Down
            } else {
                xkb::KeyDirection::Up
            };
            let changed = xkb_state.update_key(xkb::Keycode::new(key.keycode as u32), direction);
            if changed != 0 {
                vk.modifiers(
                    xkb_state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
                    xkb_state.serialize_mods(xkb::STATE_MODS_LATCHED),
                    xkb_state.serialize_mods(xkb::STATE_MODS_LOCKED),
                    xkb_state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
                );
            }
            vk.key(
                start.elapsed().as_millis() as u32,
                (key.keycode - 8) as u32,
                if key.down { 1 } else { 0 },
            );
            rt(&mut queue, &mut state)?;
        }
        Ok(())
    })();

    vk.destroy();
    let _ = queue.roundtrip(&mut state);
    result
}
