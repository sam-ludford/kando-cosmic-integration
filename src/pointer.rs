//! Pointer position query and pointer warping for compositors that expose no
//! pointer-query API.
//!
//! A transparent, fullscreen `zwlr_layer_shell_v1` overlay is mapped on every output.
//! The compositor sends `wl_pointer.enter` (or `wl_touch.down`) to whichever overlay is
//! under the cursor. Its surface-local coordinates plus the output's logical position
//! give the global pointer position. While the overlay has pointer focus,
//! `wp_pointer_warp_v1` can move the pointer relative to it. The overlays are destroyed
//! immediately afterwards.

use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat,
    wl_shm, wl_shm_pool, wl_surface, wl_touch,
};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::wp::pointer_warp::v1::client::wp_pointer_warp_v1;
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1, zxdg_output_v1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1, zwlr_layer_surface_v1,
};

#[derive(Debug, Clone, Default)]
pub struct OutputInfo {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone)]
pub struct PointerInfo {
    /// Global logical coordinates.
    pub x: f64,
    pub y: f64,
    /// Output the pointer is on.
    pub output: OutputInfo,
    /// The output's area not covered by panels (exclusive zones). Equals the output
    /// geometry if it could not be measured.
    pub work_area: Rect,
    /// Time from mapping the overlays until the enter event arrived.
    pub elapsed: Duration,
}

#[derive(Debug)]
pub enum Error {
    Connect(String),
    MissingGlobal(&'static str),
    NoInputDevice,
    NoOutputs,
    Timeout(Duration),
    Wayland(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connect(e) => write!(f, "cannot connect to Wayland display: {e}"),
            Error::MissingGlobal(g) => write!(f, "compositor does not expose {g}"),
            Error::NoInputDevice => write!(f, "seat has neither pointer nor touch capability"),
            Error::NoOutputs => write!(f, "compositor reported no outputs"),
            Error::Timeout(t) => write!(f, "no pointer enter event within {t:?}"),
            Error::Wayland(e) => write!(f, "wayland error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayKind {
    /// Covers the whole output (exclusive zone -1): surface-local == output-local.
    Full,
    /// Respects panels' exclusive zones: its size is the work area.
    WorkArea,
}

struct Overlay {
    output_id: u32,
    kind: OverlayKind,
    surface: wl_surface::WlSurface,
    layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    buffer: Option<wl_buffer::WlBuffer>,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Copy)]
struct Hit {
    output_id: u32,
    #[allow(dead_code)]
    kind: OverlayKind,
    x: f64,
    y: f64,
    /// Serial of the enter event; required by wp_pointer_warp_v1.
    serial: u32,
}

#[derive(Default)]
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    xdg_output_manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    pointer_warp: Option<wp_pointer_warp_v1::WpPointerWarpV1>,
    outputs: HashMap<u32, (wl_output::WlOutput, OutputInfo)>,
    has_pointer: bool,
    has_touch: bool,
    overlays: Vec<Overlay>,
    /// Enter on a Full overlay.
    hit: Option<Hit>,
    /// Enter on a WorkArea overlay (only after the Full overlays are gone).
    work_hit: Option<Hit>,
}

impl State {
    fn overlay_for_surface(&self, surface: &wl_surface::WlSurface) -> Option<&Overlay> {
        self.overlays.iter().find(|o| o.surface == *surface)
    }
}

fn create_buffer(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<State>,
    width: u32,
    height: u32,
) -> Result<wl_buffer::WlBuffer, Error> {
    let stride = width * 4;
    let size = (stride * height) as i64;
    let fd = unsafe { libc::memfd_create(c"kando-pointer-probe".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(Error::Wayland("memfd_create failed".into()));
    }
    let fd: OwnedFd = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::ftruncate(fd.as_raw_fd(), size) } < 0 {
        return Err(Error::Wayland("ftruncate failed".into()));
    }
    // memfd memory is zero-initialised, i.e. fully transparent ARGB.
    let pool = shm.create_pool(fd.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        stride as i32,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );
    pool.destroy();
    Ok(buffer)
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
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "zxdg_output_manager_v1" => {
                    state.xdg_output_manager =
                        Some(registry.bind(name, version.min(3), qh, ()));
                }
                "wp_pointer_warp_v1" => {
                    state.pointer_warp = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_output" => {
                    let output: wl_output::WlOutput =
                        registry.bind(name, version.min(4), qh, name);
                    state.outputs.insert(name, (output, OutputInfo::default()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let has_xdg = state.xdg_output_manager.is_some();
        if let Some((_, info)) = state.outputs.get_mut(id) {
            match event {
                wl_output::Event::Name { name } => info.name = name,
                // Fallback if xdg_output is unavailable.
                wl_output::Event::Geometry { x, y, .. } if !has_xdg => {
                    info.x = x;
                    info.y = y;
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, u32> for State {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some((_, info)) = state.outputs.get_mut(id) {
            match event {
                zxdg_output_v1::Event::LogicalPosition { x, y } => {
                    info.x = x;
                    info.y = y;
                }
                zxdg_output_v1::Event::LogicalSize { width, height } => {
                    info.width = width;
                    info.height = height;
                }
                zxdg_output_v1::Event::Name { name } if info.name.is_empty() => info.name = name,
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            state.has_pointer = caps.contains(wl_seat::Capability::Pointer);
            state.has_touch = caps.contains(wl_seat::Capability::Touch);
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, u32> for State {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, width, height } = event {
            layer.ack_configure(serial);
            let Some(shm) = state.shm.clone() else { return };
            let Some(overlay) = state.overlays.iter_mut().find(|o| o.layer == *layer) else {
                return;
            };
            let (width, height) = (width.max(1), height.max(1));
            if overlay.buffer.is_none() || overlay.width != width || overlay.height != height {
                if let Some(old) = overlay.buffer.take() {
                    old.destroy();
                }
                match create_buffer(&shm, qh, width, height) {
                    Ok(b) => overlay.buffer = Some(b),
                    Err(e) => {
                        eprintln!("kando-cosmic-helper: {e}");
                        return;
                    }
                }
                overlay.width = width;
                overlay.height = height;
            }
            overlay.surface.attach(overlay.buffer.as_ref(), 0, 0);
            overlay.surface.damage_buffer(0, 0, width as i32, height as i32);
            overlay.surface.commit();
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, surface, surface_x, surface_y } => {
                if let Some(o) = state.overlay_for_surface(&surface) {
                    let hit = Hit { output_id: o.output_id, kind: o.kind, x: surface_x, y: surface_y, serial };
                    match o.kind {
                        OverlayKind::Full if state.hit.is_none() => state.hit = Some(hit),
                        OverlayKind::WorkArea if state.work_hit.is_none() => state.work_hit = Some(hit),
                        _ => {}
                    }
                }
            }
            // Keeps the reported position current after a warp.
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                if let Some(hit) = state.hit.as_mut() {
                    hit.x = surface_x;
                    hit.y = surface_y;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_touch::WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_touch::Event::Down { serial, surface, x, y, .. } = event {
            if let Some(o) = state.overlay_for_surface(&surface) {
                let hit = Hit { output_id: o.output_id, kind: o.kind, x, y, serial };
                match o.kind {
                    OverlayKind::Full if state.hit.is_none() => state.hit = Some(hit),
                    OverlayKind::WorkArea if state.work_hit.is_none() => state.work_hit = Some(hit),
                    _ => {}
                }
            }
        }
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(State: ignore zxdg_output_manager_v1::ZxdgOutputManagerV1);
delegate_noop!(State: ignore wp_pointer_warp_v1::WpPointerWarpV1);
delegate_noop!(State: ignore wl_keyboard::WlKeyboard);

/// A mapped set of probe overlays with pointer focus on one of them.
struct Probe {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    pointer: Option<wl_pointer::WlPointer>,
    touch: Option<wl_touch::WlTouch>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    xdg_outputs: Vec<zxdg_output_v1::ZxdgOutputV1>,
    hit: Hit,
    elapsed: Duration,
}

impl Probe {
    /// Map the probe overlays. With `take_keyboard_focus`, the overlays request
    /// exclusive keyboard interactivity: cosmic-comp only honours
    /// wp_pointer_warp_v1 for the keyboard-focused surface. Focus returns to the
    /// previous surface on unmap.
    fn map(timeout: Duration, take_keyboard_focus: bool, measure_work_area: bool) -> Result<Self, Error> {
        let conn = Connection::connect_to_env().map_err(|e| Error::Connect(e.to_string()))?;
        let mut queue = conn.new_event_queue::<State>();
        let qh = queue.handle();
        let _registry = conn.display().get_registry(&qh, ());

        let mut state = State::default();
        queue.roundtrip(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;

        let compositor =
            state.compositor.clone().ok_or(Error::MissingGlobal("wl_compositor"))?;
        state.shm.as_ref().ok_or(Error::MissingGlobal("wl_shm"))?;
        let seat = state.seat.clone().ok_or(Error::MissingGlobal("wl_seat"))?;
        let layer_shell =
            state.layer_shell.clone().ok_or(Error::MissingGlobal("zwlr_layer_shell_v1"))?;
        if state.outputs.is_empty() {
            return Err(Error::NoOutputs);
        }

        // Output geometry and seat capabilities arrive with the second roundtrip.
        let xdg_outputs: Vec<_> = state
            .xdg_output_manager
            .as_ref()
            .map(|m| {
                state.outputs.iter().map(|(id, (o, _))| m.get_xdg_output(o, &qh, *id)).collect()
            })
            .unwrap_or_default();
        queue.roundtrip(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;

        if !state.has_pointer && !state.has_touch {
            return Err(Error::NoInputDevice);
        }
        let pointer = state.has_pointer.then(|| seat.get_pointer(&qh, ()));
        let touch = state.has_touch.then(|| seat.get_touch(&qh, ()));
        let keyboard = take_keyboard_focus.then(|| seat.get_keyboard(&qh, ()));

        // One overlay per output so multi-monitor setups work regardless of which
        // output the compositor considers "active".
        let ids: Vec<u32> = state.outputs.keys().copied().collect();
        let kinds: &[OverlayKind] = if measure_work_area {
            // Later layer surfaces stack above earlier ones, so the Full overlay ends
            // up on top and receives the pointer first.
            &[OverlayKind::WorkArea, OverlayKind::Full]
        } else {
            &[OverlayKind::Full]
        };
        for kind in kinds {
            for id in &ids {
                let output = state.outputs[id].0.clone();
                let surface = compositor.create_surface(&qh, ());
                let layer = layer_shell.get_layer_surface(
                    &surface,
                    Some(&output),
                    zwlr_layer_shell_v1::Layer::Overlay,
                    "kando-pointer-probe".into(),
                    &qh,
                    *id,
                );
                layer.set_size(0, 0);
                layer.set_anchor(
                    zwlr_layer_surface_v1::Anchor::Top
                        | zwlr_layer_surface_v1::Anchor::Bottom
                        | zwlr_layer_surface_v1::Anchor::Left
                        | zwlr_layer_surface_v1::Anchor::Right,
                );
                layer.set_exclusive_zone(match kind {
                    // Cover the whole output, ignoring panels: surface-local == output-local.
                    OverlayKind::Full => -1,
                    // Sized to the area left over by panels.
                    OverlayKind::WorkArea => 0,
                });
                layer.set_keyboard_interactivity(if take_keyboard_focus && *kind == OverlayKind::Full {
                    zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive
                } else {
                    zwlr_layer_surface_v1::KeyboardInteractivity::None
                });
                surface.commit();
                state.overlays.push(Overlay {
                    output_id: *id,
                    kind: *kind,
                    surface,
                    layer,
                    buffer: None,
                    width: 0,
                    height: 0,
                });
            }
        }

        let start = Instant::now();
        let waited =
            crate::util::dispatch_until(&conn, &mut queue, &mut state, timeout, |s| s.hit.is_some());
        let elapsed = start.elapsed();

        let mut probe = Probe {
            conn,
            queue,
            state,
            pointer,
            touch,
            keyboard,
            xdg_outputs,
            hit: Hit { output_id: 0, kind: OverlayKind::Full, x: 0.0, y: 0.0, serial: 0 },
            elapsed,
        };
        match waited.and_then(|_| probe.state.hit.ok_or(Error::Timeout(timeout))) {
            Ok(hit) => {
                probe.hit = hit;
                Ok(probe)
            }
            Err(e) => {
                probe.unmap();
                Err(e)
            }
        }
    }

    fn info(&self) -> PointerInfo {
        let hit = self.state.hit.unwrap_or(self.hit);
        let output =
            self.state.outputs.get(&hit.output_id).map(|(_, i)| i.clone()).unwrap_or_default();
        let x = output.x as f64 + hit.x;
        let y = output.y as f64 + hit.y;
        let work_area = self.work_area(&output, x, y);
        PointerInfo { x, y, output, work_area, elapsed: self.elapsed }
    }

    /// Work area of `output`: size from the WorkArea overlay's configure, origin from
    /// the pointer entering it (global minus surface-local). Falls back to the output
    /// geometry / origin when it could not be measured.
    fn work_area(&self, output: &OutputInfo, gx: f64, gy: f64) -> Rect {
        let mut rect = Rect { x: output.x, y: output.y, width: output.width, height: output.height };
        let overlay = self
            .state
            .overlays
            .iter()
            .find(|o| o.kind == OverlayKind::WorkArea && o.output_id == self.hit.output_id);
        if let Some(o) = overlay.filter(|o| o.width > 0 && o.height > 0) {
            rect.width = o.width as i32;
            rect.height = o.height as i32;
            if let Some(wh) = self.state.work_hit.filter(|h| h.output_id == self.hit.output_id) {
                rect.x = (gx - wh.x).round() as i32;
                rect.y = (gy - wh.y).round() as i32;
            }
        }
        rect
    }

    /// Remove the Full overlays so the pointer enters the WorkArea overlay underneath,
    /// which reveals the work area's origin. Cheap: the compositor is already awake.
    fn measure_work_area(&mut self, timeout: Duration) {
        let mut remaining = Vec::new();
        for o in self.state.overlays.drain(..) {
            if o.kind == OverlayKind::Full {
                o.layer.destroy();
                o.surface.destroy();
                if let Some(b) = o.buffer {
                    b.destroy();
                }
            } else {
                remaining.push(o);
            }
        }
        self.state.overlays = remaining;
        let result = crate::util::dispatch_until(&self.conn, &mut self.queue, &mut self.state, timeout, |s| {
            s.work_hit.is_some()
        });
        if std::env::var_os("KANDO_HELPER_DEBUG").is_some() {
            eprintln!("debug: work-area probe: {:?} hit={:?}", result.err(), self.state.work_hit);
        }
    }

    /// Warp the pointer by (dx, dy), clamped to the output the pointer is on.
    fn warp(&mut self, dx: f64, dy: f64) -> Result<(), Error> {
        let warp = self.state.pointer_warp.clone().ok_or(Error::MissingGlobal("wp_pointer_warp_v1"))?;
        let pointer = self.pointer.clone().ok_or(Error::NoInputDevice)?;
        let overlay = self
            .state
            .overlays
            .iter()
            .find(|o| o.output_id == self.hit.output_id)
            .ok_or_else(|| Error::Wayland("overlay vanished".into()))?;
        let x = (self.hit.x + dx).clamp(0.0, overlay.width.saturating_sub(1) as f64);
        let y = (self.hit.y + dy).clamp(0.0, overlay.height.saturating_sub(1) as f64);
        warp.warp_pointer(&overlay.surface, &pointer, x, y, self.hit.serial);
        // The compositor answers with a motion event carrying the new position.
        self.queue.roundtrip(&mut self.state).map_err(|e| Error::Wayland(e.to_string()))?;
        Ok(())
    }

    fn unmap(&mut self) {
        if let Some(p) = self.pointer.take() {
            p.release();
        }
        if let Some(t) = self.touch.take() {
            t.release();
        }
        if let Some(k) = self.keyboard.take() {
            k.release();
        }
        for o in self.state.overlays.drain(..) {
            o.layer.destroy();
            o.surface.destroy();
            if let Some(b) = o.buffer {
                b.destroy();
            }
        }
        for x in self.xdg_outputs.drain(..) {
            x.destroy();
        }
        let _ = self.queue.roundtrip(&mut self.state);
        let _ = self.conn.flush();
    }
}

/// Query the global pointer position. Blocks for at most `timeout`.
pub fn query_pointer(timeout: Duration) -> Result<PointerInfo, Error> {
    let mut probe = Probe::map(timeout, false, false)?;
    let info = probe.info();
    probe.unmap();
    Ok(info)
}

/// Like `query_pointer`, but also measures the panel-adjusted work area of the output
/// the pointer is on.
pub fn query_pointer_and_work_area(timeout: Duration) -> Result<PointerInfo, Error> {
    let mut probe = Probe::map(timeout, false, true)?;
    probe.measure_work_area(Duration::from_millis(1000));
    let info = probe.info();
    probe.unmap();
    Ok(info)
}

/// Move the pointer by (dx, dy) and return its new position.
pub fn move_pointer(dx: f64, dy: f64, timeout: Duration) -> Result<PointerInfo, Error> {
    let mut probe = Probe::map(timeout, true, false)?;
    let result = probe.warp(dx, dy).map(|_| probe.info());
    probe.unmap();
    result
}
