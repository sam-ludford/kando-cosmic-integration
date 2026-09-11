// SPDX-FileCopyrightText: Sam Ludford <samludford76@gmail.com>
// SPDX-License-Identifier: MIT

use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use wayland_client::{Connection, EventQueue};

use crate::pointer::Error;

/// Dispatch events until `done(state)` returns true or `timeout` elapses.
pub fn dispatch_until<S>(
    conn: &Connection,
    queue: &mut EventQueue<S>,
    state: &mut S,
    timeout: Duration,
    mut done: impl FnMut(&S) -> bool,
) -> Result<(), Error> {
    let start = Instant::now();
    loop {
        queue
            .dispatch_pending(state)
            .map_err(|e| Error::Wayland(e.to_string()))?;
        if done(state) {
            return Ok(());
        }
        let remaining = timeout
            .checked_sub(start.elapsed())
            .ok_or(Error::Timeout(timeout))?;
        conn.flush().map_err(|e| Error::Wayland(e.to_string()))?;
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let mut pfd = libc::pollfd {
            fd: guard.connection_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
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
