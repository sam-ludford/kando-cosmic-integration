mod pointer;
mod toplevel;
mod util;

use std::time::Duration;

fn usage() -> ! {
    eprintln!("usage: kando-cosmic-helper pointer [timeout-ms] | windows | focused | focus <app_id> <title>");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("pointer") => {
            let timeout = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(500u64);
            match pointer::query_pointer(Duration::from_millis(timeout)) {
                Ok(p) => println!(
                    "x={} y={} output={} ({},{} {}x{}) elapsed={:?}",
                    p.x, p.y, p.output.name, p.output.x, p.output.y, p.output.width, p.output.height, p.elapsed
                ),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("windows") => match toplevel::list_toplevels() {
            Ok(list) => {
                for t in list {
                    println!("{}\t{:?}\t{:?}", if t.activated { "*" } else { " " }, t.app_id, t.title);
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },
        Some("focused") => match toplevel::focused_toplevel() {
            Ok(Some(t)) => println!("{:?}\t{:?}", t.app_id, t.title),
            Ok(None) => println!("(no focused toplevel)"),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },
        Some("focus") => {
            let app_id = args.get(1).cloned().unwrap_or_default();
            let title = args.get(2).cloned().unwrap_or_default();
            match toplevel::focus_toplevel(&app_id, &title) {
                Ok(true) => {}
                Ok(false) => {
                    eprintln!("no matching window");
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        _ => usage(),
    }
}
