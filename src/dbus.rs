// SPDX-FileCopyrightText: Sam Ludford <samludford76@gmail.com>
// SPDX-License-Identifier: MIT

//! DBus front-end. Bus name `menu.kando.CosmicIntegration`, object path
//! `/menu/kando/CosmicIntegration`, interface `menu.kando.CosmicIntegration1`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::fdo;

use crate::keyboard::{self, KeyEvent};
use crate::pointer;
use crate::toplevel;

pub const BUS_NAME: &str = "menu.kando.CosmicIntegration";
pub const OBJECT_PATH: &str = "/menu/kando/CosmicIntegration";

/// Measured work areas per output name, with the time of measurement.
pub type WorkAreaCache = Arc<Mutex<HashMap<String, (Instant, pointer::Rect)>>>;

/// Measuring the work area costs a few hundred milliseconds, so it is cached this long.
const WORK_AREA_MAX_AGE: Duration = Duration::from_secs(600);

pub struct Helper {
    pub pointer_timeout: Duration,
    /// Serialises Wayland work; mapping two sets of probe overlays at once would
    /// make the enter events ambiguous.
    lock: Mutex<()>,
    work_areas: WorkAreaCache,
}

impl Helper {
    pub fn new(pointer_timeout: Duration, work_areas: WorkAreaCache) -> Self {
        Helper {
            pointer_timeout,
            lock: Mutex::new(()),
            work_areas,
        }
    }

    /// Pointer position plus the work area of its output, re-measuring the latter only
    /// when the cached value is missing or old.
    fn pointer_with_work_area(&self) -> Result<pointer::PointerInfo, pointer::Error> {
        let mut p = pointer::query_pointer(self.pointer_timeout)?;
        let cached = self
            .work_areas
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&p.output.name)
            .filter(|(at, _)| at.elapsed() < WORK_AREA_MAX_AGE)
            .map(|(_, r)| *r);
        match cached {
            Some(rect) => p.work_area = rect,
            None => {
                p = pointer::query_pointer_and_work_area(self.pointer_timeout)?;
                remember_work_area(&self.work_areas, &p);
            }
        }
        Ok(p)
    }

    fn guarded<T>(&self, f: impl FnOnce() -> Result<T, pointer::Error>) -> fdo::Result<T> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        f().map_err(|e| fdo::Error::Failed(e.to_string()))
    }
}

pub fn remember_work_area(cache: &WorkAreaCache, p: &pointer::PointerInfo) {
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(p.output.name.clone(), (Instant::now(), p.work_area));
}

#[zbus::interface(name = "menu.kando.CosmicIntegration1")]
impl Helper {
    /// Global pointer position.
    fn get_pointer(&self) -> fdo::Result<(f64, f64)> {
        self.guarded(|| pointer::query_pointer(self.pointer_timeout))
            .map(|p| (p.x, p.y))
    }

    /// Title and app id of the activated toplevel; empty strings if there is none.
    fn get_focused_window(&self) -> fdo::Result<(String, String)> {
        self.guarded(toplevel::focused_toplevel)
            .map(|t| t.map(|t| (t.title, t.app_id)).unwrap_or_default())
    }

    /// Everything Kando needs to open a menu, in one call:
    /// (windowTitle, appId, pointerX, pointerY, workAreaX, workAreaY, workAreaWidth, workAreaHeight)
    #[zbus(name = "GetWMInfo")]
    // The tuple must be spelled out here: zbus turns a literal tuple into eight
    // separate out-arguments (signature `ssddiiii`), which is what Kando destructures.
    // A type alias would become a single struct argument `(ssddiiii)`.
    #[allow(clippy::type_complexity)]
    fn get_wm_info(&self) -> fdo::Result<(String, String, f64, f64, i32, i32, i32, i32)> {
        self.guarded(|| {
            let window = toplevel::focused_toplevel()?.unwrap_or_default();
            let p = self.pointer_with_work_area()?;
            let w = p.work_area;
            Ok((
                window.title,
                window.app_id,
                p.x,
                p.y,
                w.x,
                w.y,
                w.width,
                w.height,
            ))
        })
    }

    /// All open toplevels as (title, appId) pairs.
    fn get_open_windows(&self) -> fdo::Result<Vec<(String, String)>> {
        self.guarded(toplevel::list_toplevels)
            .map(|l| l.into_iter().map(|t| (t.title, t.app_id)).collect())
    }

    /// Activate the first window matching title and app id. Returns false if none matched.
    fn focus_window(&self, title: String, app_id: String) -> fdo::Result<bool> {
        self.guarded(|| toplevel::focus_toplevel(&app_id, &title))
    }

    /// Move the pointer by (dx, dy). Returns the new position.
    fn move_pointer(&self, dx: f64, dy: f64) -> fdo::Result<(f64, f64)> {
        self.guarded(|| pointer::move_pointer(dx, dy, self.pointer_timeout))
            .map(|p| (p.x, p.y))
    }

    /// Simulate key events: (x11Keycode, down, delayMs) per entry, same shape as the
    /// GNOME Shell extension's SimulateKeys.
    fn simulate_keys(&self, keys: Vec<(i32, bool, i32)>) -> fdo::Result<()> {
        let keys: Vec<KeyEvent> = keys
            .into_iter()
            .map(|(keycode, down, delay_ms)| KeyEvent {
                keycode,
                down,
                delay_ms,
            })
            .collect();
        self.guarded(|| keyboard::simulate_keys(&keys))
    }
}

/// Own the bus name and serve forever.
pub fn serve(helper: Helper) -> zbus::Result<()> {
    let _conn = zbus::blocking::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, helper)?
        .build()?;
    eprintln!("kando-cosmic-helper: serving {BUS_NAME} at {OBJECT_PATH}");
    loop {
        std::thread::park();
    }
}
