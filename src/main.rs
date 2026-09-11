mod dbus;
mod keyboard;
mod pointer;
mod toplevel;
mod util;

use std::time::Duration;

const USAGE: &str = "\
usage: kando-cosmic-helper [daemon] [--exit-with-parent] [--pointer-timeout-ms N]
       kando-cosmic-helper pointer [timeout-ms]
       kando-cosmic-helper workarea [timeout-ms]
       kando-cosmic-helper move <dx> <dy>
       kando-cosmic-helper windows
       kando-cosmic-helper focused
       kando-cosmic-helper focus <app_id> <title>
       kando-cosmic-helper state <app_id> <title> fullscreen|unfullscreen|maximize|unmaximize
       kando-cosmic-helper keys <x11keycode:down|up[:delay-ms]>...";

fn fail(e: impl std::fmt::Display) -> ! {
    eprintln!("error: {e}");
    std::process::exit(1);
}

fn print_pointer(p: &pointer::PointerInfo) {
    println!(
        "x={} y={} output={} ({},{} {}x{}) workarea=({},{} {}x{}) elapsed={:?}",
        p.x, p.y, p.output.name, p.output.x, p.output.y, p.output.width, p.output.height,
        p.work_area.x, p.work_area.y, p.work_area.width, p.work_area.height, p.elapsed
    );
}

fn daemon(args: &[String]) {
    let mut timeout = Duration::from_millis(500);
    let mut exit_with_parent = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--exit-with-parent" => exit_with_parent = true,
            "--pointer-timeout-ms" => {
                i += 1;
                timeout = Duration::from_millis(
                    args.get(i).and_then(|s| s.parse().ok()).unwrap_or_else(|| fail(USAGE)),
                );
            }
            _ => fail(USAGE),
        }
        i += 1;
    }
    if exit_with_parent {
        // Die with the process that spawned us (Kando) instead of lingering on the bus.
        unsafe {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            if libc::getppid() == 1 {
                std::process::exit(0);
            }
        }
    }
    // Kando's menu window must cover the work area; cosmic-comp clamps floating windows
    // to two thirds of the output, so maximize it whenever it maps.
    std::thread::spawn(|| {
        toplevel::auto_maximize_forever("menu.kando.Kando".into(), "Kando Menu".into())
    });

    // Measure the work area once up front so the first menu opens fast.
    let work_areas: dbus::WorkAreaCache = Default::default();
    {
        let cache = work_areas.clone();
        std::thread::spawn(move || match pointer::query_pointer_and_work_area(Duration::from_secs(2)) {
            Ok(p) => dbus::remember_work_area(&cache, &p),
            Err(e) => eprintln!("kando-cosmic-helper: initial work-area probe failed: {e}"),
        });
    }

    if let Err(e) = dbus::serve(dbus::Helper::new(timeout, work_areas)) {
        fail(e);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => daemon(&[]),
        Some("daemon") => daemon(&args[1..]),
        Some(flag) if flag.starts_with("--") => daemon(&args),
        Some("pointer") => {
            let timeout = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(500u64);
            match pointer::query_pointer(Duration::from_millis(timeout)) {
                Ok(p) => print_pointer(&p),
                Err(e) => fail(e),
            }
        }
        Some("workarea") => {
            let timeout = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(500u64);
            match pointer::query_pointer_and_work_area(Duration::from_millis(timeout)) {
                Ok(p) => print_pointer(&p),
                Err(e) => fail(e),
            }
        }
        Some("move") => {
            let dx: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| fail(USAGE));
            let dy: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or_else(|| fail(USAGE));
            match pointer::move_pointer(dx, dy, Duration::from_millis(500)) {
                Ok(p) => print_pointer(&p),
                Err(e) => fail(e),
            }
        }
        Some("windows") => match toplevel::list_toplevels() {
            Ok(list) => {
                for t in list {
                    println!("{}\t{:?}\t{:?}", if t.activated { "*" } else { " " }, t.app_id, t.title);
                }
            }
            Err(e) => fail(e),
        },
        Some("focused") => match toplevel::focused_toplevel() {
            Ok(Some(t)) => println!("{:?}\t{:?}", t.app_id, t.title),
            Ok(None) => println!("(no focused toplevel)"),
            Err(e) => fail(e),
        },
        Some("focus") => {
            let app_id = args.get(1).cloned().unwrap_or_default();
            let title = args.get(2).cloned().unwrap_or_default();
            match toplevel::focus_toplevel(&app_id, &title) {
                Ok(true) => {}
                Ok(false) => fail("no matching window"),
                Err(e) => fail(e),
            }
        }
        Some("state") => {
            let app_id = args.get(1).cloned().unwrap_or_default();
            let title = args.get(2).cloned().unwrap_or_default();
            let change = match args.get(3).map(String::as_str) {
                Some("fullscreen") => toplevel::StateChange::Fullscreen,
                Some("unfullscreen") => toplevel::StateChange::Unfullscreen,
                Some("maximize") => toplevel::StateChange::Maximize,
                Some("unmaximize") => toplevel::StateChange::Unmaximize,
                _ => fail(USAGE),
            };
            match toplevel::set_toplevel_state(&app_id, &title, change) {
                Ok(true) => {}
                Ok(false) => fail("no matching window"),
                Err(e) => fail(e),
            }
        }
        Some("keys") => {
            let keys: Vec<keyboard::KeyEvent> = args[1..]
                .iter()
                .map(|spec| {
                    let parts: Vec<&str> = spec.split(':').collect();
                    let keycode = parts.first().and_then(|s| s.parse().ok()).unwrap_or_else(|| fail(USAGE));
                    let down = match parts.get(1) {
                        Some(&"down") => true,
                        Some(&"up") => false,
                        _ => fail(USAGE),
                    };
                    let delay_ms = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                    keyboard::KeyEvent { keycode, down, delay_ms }
                })
                .collect();
            if let Err(e) = keyboard::simulate_keys(&keys) {
                fail(e);
            }
        }
        _ => fail(USAGE),
    }
}
