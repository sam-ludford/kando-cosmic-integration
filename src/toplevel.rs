//! Toplevel (window) listing, focused-window lookup and activation via
//! `ext_foreign_toplevel_list_v1` + `zcosmic_toplevel_info_v1` / `zcosmic_toplevel_manager_v1`.

use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1, zcosmic_toplevel_info_v1,
};
use cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1;
use wayland_client::protocol::{wl_output, wl_registry, wl_seat};
use wayland_client::{
    delegate_noop, event_created_child, Connection, Dispatch, QueueHandle,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};

pub use crate::pointer::Error;

const DONE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);

#[derive(Debug, Clone, Default)]
pub struct Toplevel {
    pub title: String,
    pub app_id: String,
    pub activated: bool,
}

struct Entry {
    info: Toplevel,
    ext: ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    cosmic: Option<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1>,
    closed: bool,
    maximize_sent: bool,
}

#[derive(Default)]
struct State {
    seat: Option<wl_seat::WlSeat>,
    list: Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    info: Option<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1>,
    manager: Option<zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1>,
    entries: Vec<Entry>,
    /// Set once zcosmic_toplevel_info_v1.done has been received, i.e. the
    /// compositor has flushed the initial state of every cosmic handle.
    done: bool,
    /// (app_id, title) of windows to maximize as soon as they appear.
    auto_maximize: Option<(String, String)>,
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
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                // Bound so the compositor may reference outputs in handle events.
                "wl_output" => {
                    let _: wl_output::WlOutput = registry.bind(name, version.min(4), qh, ());
                }
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "ext_foreign_toplevel_list_v1" => {
                    state.list = Some(registry.bind(name, 1, qh, ()));
                }
                // Version 2 is required for get_cosmic_toplevel.
                "zcosmic_toplevel_info_v1" if version >= 2 => {
                    state.info = Some(registry.bind(name, version.min(3), qh, ()));
                }
                "zcosmic_toplevel_manager_v1" => {
                    state.manager = Some(registry.bind(name, version.min(4), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            let cosmic = state.info.as_ref().map(|i| i.get_cosmic_toplevel(&toplevel, qh, ()));
            state.entries.push(Entry {
                info: Toplevel::default(),
                ext: toplevel,
                cosmic,
                closed: false,
                maximize_sent: false,
            });
        }
    }

    event_created_child!(State, ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let manager = state.manager.clone();
        let rule = state.auto_maximize.clone();
        let Some(entry) = state.entries.iter_mut().find(|e| e.ext == *handle) else { return };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => entry.info.title = title,
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => entry.info.app_id = app_id,
            ext_foreign_toplevel_handle_v1::Event::Closed => entry.closed = true,
            // `done` follows the initial title/app_id, i.e. the window has just mapped.
            ext_foreign_toplevel_handle_v1::Event::Done => {
                if let (Some((app_id, title)), Some(manager), Some(cosmic)) =
                    (rule, manager, entry.cosmic.as_ref())
                {
                    if !entry.maximize_sent && entry.info.app_id == app_id && entry.info.title == title {
                        manager.set_maximized(cosmic);
                        entry.maximize_sent = true;
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
        event: zcosmic_toplevel_info_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Only the deprecated v1 `toplevel` event creates children; we bind v2+.
        if let zcosmic_toplevel_info_v1::Event::Done = event {
            state.done = true;
        }
    }

    event_created_child!(State, zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()),
    ]);
}

impl Dispatch<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(entry) = state.entries.iter_mut().find(|e| e.cosmic.as_ref() == Some(handle)) else {
            return;
        };
        if let zcosmic_toplevel_handle_v1::Event::State { state: raw } = event {
            entry.info.activated = raw
                .chunks_exact(4)
                .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                .any(|v| v == zcosmic_toplevel_handle_v1::State::Activated as u32);
        }
    }
}

delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore wl_output::WlOutput);
delegate_noop!(State: ignore zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1);

struct Session {
    queue: wayland_client::EventQueue<State>,
    state: State,
}

impl Session {
    fn open() -> Result<Self, Error> {
        let conn = Connection::connect_to_env().map_err(|e| Error::Connect(e.to_string()))?;
        let mut queue = conn.new_event_queue::<State>();
        let qh = queue.handle();
        let _registry = conn.display().get_registry(&qh, ());
        let mut state = State::default();
        let rt = |q: &mut wayland_client::EventQueue<State>, s: &mut State| {
            q.roundtrip(s).map(|_| ()).map_err(|e| Error::Wayland(e.to_string()))
        };
        rt(&mut queue, &mut state)?; // globals
        if state.list.is_none() {
            return Err(Error::MissingGlobal("ext_foreign_toplevel_list_v1"));
        }
        if state.info.is_none() {
            return Err(Error::MissingGlobal("zcosmic_toplevel_info_v1 (v2+)"));
        }
        rt(&mut queue, &mut state)?; // toplevel handles + ext properties
        // cosmic-comp sends the cosmic handles' state on its next refresh, not
        // synchronously, so wait for the `done` event rather than a roundtrip.
        crate::util::dispatch_until(&conn, &mut queue, &mut state, DONE_TIMEOUT, |s| s.done)?;
        Ok(Session { queue, state })
    }

    fn close(mut self) {
        for e in self.state.entries.drain(..) {
            if let Some(c) = e.cosmic {
                c.destroy();
            }
            e.ext.destroy();
        }
        if let Some(l) = &self.state.list {
            l.stop();
        }
        let _ = self.queue.roundtrip(&mut self.state);
    }

    fn toplevels(&self) -> Vec<Toplevel> {
        self.state.entries.iter().filter(|e| !e.closed).map(|e| e.info.clone()).collect()
    }
}

/// All open toplevels in compositor order.
pub fn list_toplevels() -> Result<Vec<Toplevel>, Error> {
    let s = Session::open()?;
    let list = s.toplevels();
    s.close();
    Ok(list)
}

/// The toplevel carrying the `activated` state, if any.
pub fn focused_toplevel() -> Result<Option<Toplevel>, Error> {
    Ok(list_toplevels()?.into_iter().find(|t| t.activated))
}

/// Window state changes available through zcosmic_toplevel_manager_v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateChange {
    Fullscreen,
    Unfullscreen,
    Maximize,
    Unmaximize,
    Close,
}

/// Apply `change` to the first toplevel matching `app_id` and `title` (empty strings
/// match any). Returns false if nothing matched.
pub fn set_toplevel_state(app_id: &str, title: &str, change: StateChange) -> Result<bool, Error> {
    let mut s = Session::open()?;
    let manager =
        s.state.manager.clone().ok_or(Error::MissingGlobal("zcosmic_toplevel_manager_v1"))?;
    let target = s.state.entries.iter().find(|e| {
        !e.closed
            && (app_id.is_empty() || e.info.app_id == app_id)
            && (title.is_empty() || e.info.title == title)
    });
    let found = match target.and_then(|e| e.cosmic.clone()) {
        Some(handle) => {
            match change {
                StateChange::Fullscreen => manager.set_fullscreen(&handle, None),
                StateChange::Unfullscreen => manager.unset_fullscreen(&handle),
                StateChange::Maximize => manager.set_maximized(&handle),
                StateChange::Unmaximize => manager.unset_maximized(&handle),
                StateChange::Close => manager.close(&handle),
            }
            true
        }
        None => false,
    };
    s.queue.roundtrip(&mut s.state).map_err(|e| Error::Wayland(e.to_string()))?;
    s.close();
    Ok(found)
}

/// Activate the first toplevel matching `app_id` and `title` (empty strings match any).
pub fn focus_toplevel(app_id: &str, title: &str) -> Result<bool, Error> {
    let mut s = Session::open()?;
    let manager = s.state.manager.clone().ok_or(Error::MissingGlobal("zcosmic_toplevel_manager_v1"))?;
    let seat = s.state.seat.clone().ok_or(Error::MissingGlobal("wl_seat"))?;
    let target = s.state.entries.iter().find(|e| {
        !e.closed
            && (app_id.is_empty() || e.info.app_id == app_id)
            && (title.is_empty() || e.info.title == title)
    });
    let found = match target.and_then(|e| e.cosmic.clone()) {
        Some(handle) => {
            manager.activate(&handle, &seat);
            true
        }
        None => false,
    };
    s.queue.roundtrip(&mut s.state).map_err(|e| Error::Wayland(e.to_string()))?;
    s.close();
    Ok(found)
}

/// Run forever on a dedicated connection: every toplevel whose app id and title match is
/// maximized right after it maps. This is how Kando's transparent menu window gets to
/// cover the whole work area on cosmic-comp, which otherwise clamps floating windows
/// to two thirds of the output and has no per-app size rules.
pub fn auto_maximize_forever(app_id: String, title: String) {
    loop {
        match auto_maximize_session(&app_id, &title) {
            Ok(()) => return,
            Err(e) => {
                eprintln!("kando-cosmic-helper: window watcher: {e}; retrying in 2s");
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }
}

fn auto_maximize_session(app_id: &str, title: &str) -> Result<(), Error> {
    let conn = Connection::connect_to_env().map_err(|e| Error::Connect(e.to_string()))?;
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut state = State { auto_maximize: Some((app_id.to_string(), title.to_string())), ..Default::default() };
    queue.roundtrip(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;
    if state.list.is_none() {
        return Err(Error::MissingGlobal("ext_foreign_toplevel_list_v1"));
    }
    if state.info.is_none() {
        return Err(Error::MissingGlobal("zcosmic_toplevel_info_v1 (v2+)"));
    }
    if state.manager.is_none() {
        return Err(Error::MissingGlobal("zcosmic_toplevel_manager_v1"));
    }
    // Windows that already exist are left alone: only mark them handled.
    queue.roundtrip(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;
    for e in state.entries.iter_mut() {
        e.maximize_sent = true;
    }
    loop {
        queue.blocking_dispatch(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;
        state.entries.retain(|e| !e.closed);
    }
}
