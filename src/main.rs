mod pointer;

use std::time::Duration;

fn usage() -> ! {
    eprintln!("usage: kando-cosmic-helper pointer [timeout-ms]");
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
        _ => usage(),
    }
}
