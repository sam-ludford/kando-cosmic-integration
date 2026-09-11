//! Workspace listing, creation and window placement via `ext_workspace_v1`,
//! `zcosmic_workspace_manager_v2` (rename/pin) and
//! `zcosmic_toplevel_manager_v1.move_to_ext_workspace`.

use std::time::Duration;

use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1, zcosmic_toplevel_info_v1,
};
use cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1;
use cosmic_protocols::workspace::v2::client::{
    zcosmic_workspace_handle_v2, zcosmic_workspace_manager_v2,
};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{delegate_noop, event_created_child, Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1, ext_workspace_handle_v1, ext_workspace_manager_v1,
};

pub use crate::pointer::Error;

const SYNC_TIMEOUT: Duration = Duration::from_millis(1500);
const FOCUS_SETTLE_TIMEOUT: Duration = Duration::from_millis(700);

/// zcosmic_workspace_handle_v2 state bit.
const PINNED: u32 = 1;

/// Persistent name -> workspace id map, one `name<TAB>id` per line in
/// `$XDG_CONFIG_HOME/kando-cosmic-helper/workspaces`.
struct NameMap {
    entries: Vec<(String, String)>,
}

impl NameMap {
    fn path() -> std::path::PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
            .unwrap_or_default();
        base.join("kando-cosmic-helper").join("workspaces")
    }

    fn load() -> Self {
        let entries = std::fs::read_to_string(Self::path())
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(n, i)| (n.to_string(), i.trim().to_string())))
            .collect();
        NameMap { entries }
    }

    fn save(&self) -> Result<(), Error> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::Wayland(e.to_string()))?;
        }
        let body: String = self.entries.iter().map(|(n, i)| format!("{n}\t{i}\n")).collect();
        std::fs::write(path, body).map_err(|e| Error::Wayland(e.to_string()))
    }

    fn id_for(&self, name: &str) -> Option<String> {
        self.entries.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, i)| i.clone())
    }

    fn name_for(&self, id: &str) -> Option<&str> {
        if id.is_empty() {
            return None;
        }
        self.entries.iter().find(|(_, i)| i == id).map(|(n, _)| n.as_str())
    }

    fn set(&mut self, name: &str, id: &str) {
        self.entries.retain(|(n, i)| !n.eq_ignore_ascii_case(name) && i != id);
        self.entries.push((name.to_string(), id.to_string()));
    }

    fn remove(&mut self, name: &str) {
        self.entries.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }
}

#[derive(Debug, Clone, Default)]
pub struct Workspace {
    pub name: String,
    pub id: String,
    pub coordinates: Vec<u32>,
    pub active: bool,
    /// Raw zcosmic_workspace_handle_v2 capabilities (rename=1, set_tiling_state=2,
    /// pin=4, move=8) and state bits (pinned=1).
    pub cosmic_caps: u32,
    pub cosmic_state: u32,
}

struct WsEntry {
    info: Workspace,
    ext: ext_workspace_handle_v1::ExtWorkspaceHandleV1,
    cosmic: Option<zcosmic_workspace_handle_v2::ZcosmicWorkspaceHandleV2>,
    removed: bool,
}

struct Group {
    handle: ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1,
    outputs: Vec<wl_output::WlOutput>,
    workspaces: Vec<ext_workspace_handle_v1::ExtWorkspaceHandleV1>,
    can_create: bool,
}

struct Top {
    ext: ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    cosmic: Option<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1>,
    app_id: String,
    title: String,
    activated: bool,
    outputs: Vec<wl_output::WlOutput>,
    closed: bool,
}

#[derive(Default)]
struct State {
    ws_manager: Option<ext_workspace_manager_v1::ExtWorkspaceManagerV1>,
    cosmic_ws_manager: Option<zcosmic_workspace_manager_v2::ZcosmicWorkspaceManagerV2>,
    top_manager: Option<zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1>,
    top_list: Option<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1>,
    top_info: Option<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1>,
    outputs: Vec<wl_output::WlOutput>,
    groups: Vec<Group>,
    workspaces: Vec<WsEntry>,
    toplevels: Vec<Top>,
    ws_done: bool,
    top_done: bool,
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
                "wl_output" => state.outputs.push(registry.bind(name, version.min(4), qh, ())),
                "ext_workspace_manager_v1" => {
                    state.ws_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zcosmic_workspace_manager_v2" if version >= 2 => {
                    state.cosmic_ws_manager = Some(registry.bind(name, 2, qh, ()));
                }
                "zcosmic_toplevel_manager_v1" if version >= 4 => {
                    state.top_manager = Some(registry.bind(name, 4, qh, ()));
                }
                "ext_foreign_toplevel_list_v1" => {
                    state.top_list = Some(registry.bind(name, 1, qh, ()));
                }
                "zcosmic_toplevel_info_v1" if version >= 2 => {
                    state.top_info = Some(registry.bind(name, version.min(3), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ext_workspace_manager_v1::ExtWorkspaceManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ext_workspace_manager_v1::ExtWorkspaceManagerV1,
        event: ext_workspace_manager_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_workspace_manager_v1::Event::WorkspaceGroup { workspace_group } => {
                state.groups.push(Group {
                    handle: workspace_group,
                    outputs: Vec::new(),
                    workspaces: Vec::new(),
                    can_create: false,
                });
            }
            ext_workspace_manager_v1::Event::Workspace { workspace } => {
                let cosmic =
                    state.cosmic_ws_manager.as_ref().map(|m| m.get_cosmic_workspace(&workspace, qh, ()));
                state.workspaces.push(WsEntry {
                    info: Workspace::default(),
                    ext: workspace,
                    cosmic,
                    removed: false,
                });
            }
            ext_workspace_manager_v1::Event::Done => state.ws_done = true,
            _ => {}
        }
    }

    event_created_child!(State, ext_workspace_manager_v1::ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ext_workspace_handle_v1::ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1,
        event: ext_workspace_group_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(group) = state.groups.iter_mut().find(|g| g.handle == *handle) else { return };
        match event {
            ext_workspace_group_handle_v1::Event::Capabilities { capabilities } => {
                group.can_create = capabilities
                    .into_result()
                    .map(|c| c.contains(ext_workspace_group_handle_v1::GroupCapabilities::CreateWorkspace))
                    .unwrap_or(false);
            }
            ext_workspace_group_handle_v1::Event::OutputEnter { output } => group.outputs.push(output),
            ext_workspace_group_handle_v1::Event::OutputLeave { output } => {
                group.outputs.retain(|o| *o != output)
            }
            ext_workspace_group_handle_v1::Event::WorkspaceEnter { workspace } => {
                group.workspaces.push(workspace)
            }
            ext_workspace_group_handle_v1::Event::WorkspaceLeave { workspace } => {
                group.workspaces.retain(|w| *w != workspace)
            }
            _ => {}
        }
    }
}

impl Dispatch<ext_workspace_handle_v1::ExtWorkspaceHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ext_workspace_handle_v1::ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(ws) = state.workspaces.iter_mut().find(|w| w.ext == *handle) else { return };
        match event {
            ext_workspace_handle_v1::Event::Id { id } => ws.info.id = id,
            ext_workspace_handle_v1::Event::Name { name } => ws.info.name = name,
            ext_workspace_handle_v1::Event::Coordinates { coordinates } => {
                ws.info.coordinates = coordinates
                    .chunks_exact(4)
                    .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
            }
            ext_workspace_handle_v1::Event::State { state: s } => {
                ws.info.active = s
                    .into_result()
                    .map(|s| s.contains(ext_workspace_handle_v1::State::Active))
                    .unwrap_or(false);
            }
            ext_workspace_handle_v1::Event::Removed => ws.removed = true,
            _ => {}
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
            let cosmic = state.top_info.as_ref().map(|i| i.get_cosmic_toplevel(&toplevel, qh, ()));
            state.toplevels.push(Top {
                ext: toplevel,
                cosmic,
                app_id: String::new(),
                title: String::new(),
                activated: false,
                outputs: Vec::new(),
                closed: false,
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
        let Some(t) = state.toplevels.iter_mut().find(|t| t.ext == *handle) else { return };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => t.title = title,
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => t.app_id = app_id,
            ext_foreign_toplevel_handle_v1::Event::Closed => t.closed = true,
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
        if let zcosmic_toplevel_info_v1::Event::Done = event {
            state.top_done = true;
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
        let Some(t) = state.toplevels.iter_mut().find(|t| t.cosmic.as_ref() == Some(handle)) else {
            return;
        };
        match event {
            zcosmic_toplevel_handle_v1::Event::State { state: raw } => {
                t.activated = raw
                    .chunks_exact(4)
                    .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .any(|v| v == zcosmic_toplevel_handle_v1::State::Activated as u32);
            }
            zcosmic_toplevel_handle_v1::Event::OutputEnter { output } => t.outputs.push(output),
            zcosmic_toplevel_handle_v1::Event::OutputLeave { output } => {
                t.outputs.retain(|o| *o != output)
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_output::WlOutput);
delegate_noop!(State: ignore zcosmic_workspace_manager_v2::ZcosmicWorkspaceManagerV2);
impl Dispatch<zcosmic_workspace_handle_v2::ZcosmicWorkspaceHandleV2, ()> for State {
    fn event(
        state: &mut Self,
        handle: &zcosmic_workspace_handle_v2::ZcosmicWorkspaceHandleV2,
        event: zcosmic_workspace_handle_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(ws) = state.workspaces.iter_mut().find(|w| w.cosmic.as_ref() == Some(handle)) else {
            return;
        };
        match event {
            zcosmic_workspace_handle_v2::Event::Capabilities { capabilities } => {
                ws.info.cosmic_caps = capabilities.into_result().map(|c| c.bits()).unwrap_or(0);
            }
            zcosmic_workspace_handle_v2::Event::State { state: s } => {
                ws.info.cosmic_state = s.into_result().map(|c| c.bits()).unwrap_or(0);
            }
            _ => {}
        }
    }
}
delegate_noop!(State: ignore zcosmic_toplevel_manager_v1::ZcosmicToplevelManagerV1);

struct Session {
    conn: Connection,
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
        queue.roundtrip(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;
        if state.ws_manager.is_none() {
            return Err(Error::MissingGlobal("ext_workspace_manager_v1"));
        }
        if state.top_list.is_none() || state.top_info.is_none() {
            return Err(Error::MissingGlobal("ext_foreign_toplevel_list_v1 / zcosmic_toplevel_info_v1"));
        }
        // Groups, workspaces and toplevels are announced now; their properties follow
        // with the respective `done` events.
        queue.roundtrip(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;
        crate::util::dispatch_until(&conn, &mut queue, &mut state, SYNC_TIMEOUT, |s| {
            s.ws_done && s.top_done
        })?;
        let mut session = Session { conn, queue, state };
        session.apply_names();
        Ok(session)
    }

    fn sync(&mut self) -> Result<(), Error> {
        self.queue.roundtrip(&mut self.state).map_err(|e| Error::Wayland(e.to_string())).map(|_| ())
    }

    fn commit(&mut self) -> Result<(), Error> {
        if let Some(m) = &self.state.ws_manager {
            m.commit();
        }
        self.sync()
    }

    fn find(&self, name: &str) -> Option<usize> {
        let mapped_id = NameMap::load().id_for(name);
        self.state.workspaces.iter().position(|w| {
            !w.removed
                && (w.info.name.eq_ignore_ascii_case(name)
                    || (!w.info.id.is_empty() && mapped_id.as_deref() == Some(w.info.id.as_str())))
        })
    }

    /// Replace compositor names ("1", "2", ...) with the helper's names where known.
    fn apply_names(&mut self) {
        let map = NameMap::load();
        for w in self.state.workspaces.iter_mut() {
            if let Some(n) = map.name_for(&w.info.id) {
                w.info.name = n.to_string();
            }
        }
    }

    /// Index of the workspace called `name`, creating it if needed.
    ///
    /// cosmic-comp (1.7) ignores both `create_workspace` and `rename`, but it always
    /// keeps one empty workspace at the end and gives pinned workspaces a stable id.
    /// So a new name pins the trailing workspace and records name -> id in the
    /// helper's own map; cosmic-comp then appends a fresh empty workspace.
    fn ensure(&mut self, name: &str) -> Result<usize, Error> {
        if let Some(i) = self.find(name) {
            return Ok(i);
        }
        let idx = self
            .state
            .workspaces
            .iter()
            .enumerate()
            .filter(|(_, w)| !w.removed && w.info.cosmic_state & PINNED == 0)
            .max_by(|(_, a), (_, b)| a.info.coordinates.cmp(&b.info.coordinates))
            .map(|(i, _)| i)
            .ok_or_else(|| Error::Wayland("no unpinned workspace left to name".into()))?;
        let cosmic = self.state.workspaces[idx]
            .cosmic
            .clone()
            .ok_or(Error::MissingGlobal("zcosmic_workspace_manager_v2 (v2+)"))?;
        cosmic.rename(name.to_string()); // honoured by future cosmic-comp versions
        cosmic.pin();
        self.commit()?;
        let _ = crate::util::dispatch_until(&self.conn, &mut self.queue, &mut self.state, SYNC_TIMEOUT, |s| {
            !s.workspaces[idx].info.id.is_empty()
        });
        let id = self.state.workspaces[idx].info.id.clone();
        if id.is_empty() {
            return Err(Error::Wayland("compositor did not pin the workspace".into()));
        }
        let mut map = NameMap::load();
        map.set(name, &id);
        map.save()?;
        self.state.workspaces[idx].info.name = name.to_string();
        Ok(idx)
    }

    fn focused_toplevel(&self) -> Option<&Top> {
        self.state
            .toplevels
            .iter()
            .find(|t| !t.closed && t.activated && !crate::toplevel::is_kando(&t.app_id))
    }

    /// Wait briefly for a non-Kando window to become activated (focus returns to the
    /// user's window a few milliseconds after Kando's menu closes).
    fn wait_for_focus(&mut self) {
        let _ = crate::util::dispatch_until(&self.conn, &mut self.queue, &mut self.state, FOCUS_SETTLE_TIMEOUT, |s| {
            s.toplevels.iter().any(|t| !t.closed && t.activated && !crate::toplevel::is_kando(&t.app_id))
        });
    }

    fn close(mut self) {
        for w in self.state.workspaces.drain(..) {
            if let Some(c) = w.cosmic {
                c.destroy();
            }
            w.ext.destroy();
        }
        for g in self.state.groups.drain(..) {
            g.handle.destroy();
        }
        for t in self.state.toplevels.drain(..) {
            if let Some(c) = t.cosmic {
                c.destroy();
            }
            t.ext.destroy();
        }
        if let Some(m) = &self.state.ws_manager {
            m.stop();
        }
        if let Some(l) = &self.state.top_list {
            l.stop();
        }
        let _ = self.queue.roundtrip(&mut self.state);
    }
}

/// All workspaces in coordinate order.
pub fn list_workspaces() -> Result<Vec<Workspace>, Error> {
    let s = Session::open()?;
    let mut list: Vec<Workspace> =
        s.state.workspaces.iter().filter(|w| !w.removed).map(|w| w.info.clone()).collect();
    list.sort_by(|a, b| a.coordinates.cmp(&b.coordinates));
    s.close();
    Ok(list)
}

/// Switch to the workspace called `name`, creating it if necessary.
pub fn goto_workspace(name: &str) -> Result<(), Error> {
    let mut s = Session::open()?;
    let i = s.ensure(name)?;
    s.state.workspaces[i].ext.activate();
    s.commit()?;
    s.close();
    Ok(())
}

/// Move the focused window to the workspace called `name` (created if necessary).
/// With `follow`, switch to that workspace too. Returns false if no window is focused.
pub fn send_focused_to_workspace(name: &str, follow: bool) -> Result<bool, Error> {
    let mut s = Session::open()?;
    let manager =
        s.state.top_manager.clone().ok_or(Error::MissingGlobal("zcosmic_toplevel_manager_v1 (v4+)"))?;
    let i = s.ensure(name)?;
    s.wait_for_focus();
    let Some(top) = s.focused_toplevel() else {
        s.close();
        return Ok(false);
    };
    let Some(cosmic) = top.cosmic.clone() else {
        s.close();
        return Ok(false);
    };
    let output = top
        .outputs
        .first()
        .cloned()
        .or_else(|| s.state.groups.iter().flat_map(|g| g.outputs.iter()).next().cloned())
        .or_else(|| s.state.outputs.first().cloned())
        .ok_or(Error::NoOutputs)?;
    manager.move_to_ext_workspace(&cosmic, &s.state.workspaces[i].ext, &output);
    s.sync()?;
    if follow {
        s.state.workspaces[i].ext.activate();
        s.commit()?;
    }
    s.close();
    Ok(true)
}

/// Unpin the workspace called `name` and forget the name. cosmic-comp removes it once
/// it is empty.
pub fn forget_workspace(name: &str) -> Result<bool, Error> {
    let mut s = Session::open()?;
    let Some(i) = s.find(name) else {
        s.close();
        return Ok(false);
    };
    if let Some(c) = &s.state.workspaces[i].cosmic {
        c.unpin();
    }
    s.commit()?;
    let mut map = NameMap::load();
    map.remove(name);
    map.save()?;
    s.close();
    Ok(true)
}
