//! Pointer position query for compositors that expose no pointer-query API.
//!
//! A transparent, fullscreen `zwlr_layer_shell_v1` overlay is mapped on every output.
//! The compositor sends `wl_pointer.enter` (or `wl_touch.down`) to whichever overlay is
//! under the cursor. Its surface-local coordinates plus the output's logical position
//! give the global pointer position. The overlays are destroyed immediately afterwards.

use std::collections::HashMap;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface, wl_touch,
};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
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

#[derive(Debug, Clone)]
pub struct PointerInfo {
    /// Global logical coordinates.
    pub x: f64,
    pub y: f64,
    /// Output the pointer is on.
    pub output: OutputInfo,
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

struct Overlay {
    output_id: u32,
    surface: wl_surface::WlSurface,
    layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    buffer: Option<wl_buffer::WlBuffer>,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    xdg_output_manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    outputs: HashMap<u32, (wl_output::WlOutput, OutputInfo)>,
    has_pointer: bool,
    has_touch: bool,
    overlays: Vec<Overlay>,
    /// (output id, surface-local x, surface-local y)
    hit: Option<(u32, f64, f64)>,
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
    if unsafe { libc::ftruncate(fd.as_fd().as_raw_fd_compat(), size) } < 0 {
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

trait RawFdCompat {
    fn as_raw_fd_compat(&self) -> i32;
}
impl RawFdCompat for std::os::fd::BorrowedFd<'_> {
    fn as_raw_fd_compat(&self) -> i32 {
        use std::os::fd::AsRawFd;
        self.as_raw_fd()
    }
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
                    state.compositor =
                        Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_seat" => {
                    if state.seat.is_none() {
                        state.seat = Some(registry.bind(name, version.min(5), qh, ()));
                    }
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "zxdg_output_manager_v1" => {
                    state.xdg_output_manager =
                        Some(registry.bind(name, version.min(3), qh, ()));
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
        if let Some((_, info)) = state.outputs.get_mut(id) {
            match event {
                wl_output::Event::Name { name } => info.name = name,
                // Fallback if xdg_output is unavailable; overwritten by logical values.
                wl_output::Event::Geometry { x, y, .. } if state.xdg_output_manager.is_none() => {
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
        if let wl_pointer::Event::Enter { surface, surface_x, surface_y, .. } = event {
            if state.hit.is_none() {
                if let Some(o) = state.overlay_for_surface(&surface) {
                    state.hit = Some((o.output_id, surface_x, surface_y));
                }
            }
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
        if let wl_touch::Event::Down { surface, x, y, .. } = event {
            if state.hit.is_none() {
                if let Some(o) = state.overlay_for_surface(&surface) {
                    state.hit = Some((o.output_id, x, y));
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

/// Query the global pointer position. Blocks for at most `timeout`.
pub fn query_pointer(timeout: Duration) -> Result<PointerInfo, Error> {
    let conn = Connection::connect_to_env().map_err(|e| Error::Connect(e.to_string()))?;
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());

    let mut state = State::default();
    queue.roundtrip(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;

    let compositor = state.compositor.clone().ok_or(Error::MissingGlobal("wl_compositor"))?;
    let shm = state.shm.clone().ok_or(Error::MissingGlobal("wl_shm"))?;
    let seat = state.seat.clone().ok_or(Error::MissingGlobal("wl_seat"))?;
    let layer_shell =
        state.layer_shell.clone().ok_or(Error::MissingGlobal("zwlr_layer_shell_v1"))?;
    if state.outputs.is_empty() {
        return Err(Error::NoOutputs);
    }
    let _ = shm;

    // Output geometry (and seat capabilities) arrive with the second roundtrip.
    let xdg_outputs: Vec<_> = state
        .xdg_output_manager
        .as_ref()
        .map(|m| {
            state
                .outputs
                .iter()
                .map(|(id, (o, _))| m.get_xdg_output(o, &qh, *id))
                .collect()
        })
        .unwrap_or_default();
    queue.roundtrip(&mut state).map_err(|e| Error::Wayland(e.to_string()))?;

    if !state.has_pointer && !state.has_touch {
        return Err(Error::NoInputDevice);
    }
    let pointer = state.has_pointer.then(|| seat.get_pointer(&qh, ()));
    let touch = state.has_touch.then(|| seat.get_touch(&qh, ()));

    // One overlay per output so multi-monitor setups work regardless of which output
    // the compositor considers "active".
    let ids: Vec<u32> = state.outputs.keys().copied().collect();
    for id in ids {
        let output = state.outputs[&id].0.clone();
        let surface = compositor.create_surface(&qh, ());
        let layer = layer_shell.get_layer_surface(
            &surface,
            Some(&output),
            zwlr_layer_shell_v1::Layer::Overlay,
            "kando-pointer-probe".into(),
            &qh,
            id,
        );
        layer.set_size(0, 0);
        layer.set_anchor(
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Bottom
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right,
        );
        // Cover the whole output, ignoring panels, so surface-local == output-local.
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        surface.commit();
        state.overlays.push(Overlay { output_id: id, surface, layer, buffer: None, width: 0, height: 0 });
    }

    let start = Instant::now();
    let result = wait_for_hit(&conn, &mut queue, &mut state, timeout);
    let elapsed = start.elapsed();

    // Tear down in the correct order: pointer/touch, layer surfaces, surfaces, buffers.
    if let Some(p) = pointer {
        p.release();
    }
    if let Some(t) = touch {
        t.release();
    }
    for o in state.overlays.drain(..) {
        o.layer.destroy();
        o.surface.destroy();
        if let Some(b) = o.buffer {
            b.destroy();
        }
    }
    for x in xdg_outputs {
        x.destroy();
    }
    let _ = queue.roundtrip(&mut state);

    let (output_id, lx, ly) = result?;
    let output = state.outputs.get(&output_id).map(|(_, i)| i.clone()).unwrap_or_default();
    Ok(PointerInfo { x: output.x as f64 + lx, y: output.y as f64 + ly, output, elapsed })
}

fn wait_for_hit(
    conn: &Connection,
    queue: &mut wayland_client::EventQueue<State>,
    state: &mut State,
    timeout: Duration,
) -> Result<(u32, f64, f64), Error> {
    use std::os::fd::AsRawFd;
    let start = Instant::now();
    loop {
        queue.dispatch_pending(state).map_err(|e| Error::Wayland(e.to_string()))?;
        if let Some(hit) = state.hit {
            return Ok(hit);
        }
        let remaining = timeout.checked_sub(start.elapsed()).ok_or(Error::Timeout(timeout))?;
        conn.flush().map_err(|e| Error::Wayland(e.to_string()))?;
        let Some(guard) = queue.prepare_read() else { continue };
        let mut pfd = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ret = unsafe { libc::poll(&mut pfd, 1, remaining.as_millis() as i32) };
        if ret > 0 {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(Error::Wayland(e.to_string())),
            }
        } else if ret == 0 {
            drop(guard);
            return Err(Error::Timeout(timeout));
        } else {
            drop(guard);
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(Error::Wayland(err.to_string()));
            }
        }
    }
}
