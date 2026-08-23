//! Where does a position read's time actually go?
//!
//!     cargo run --example read_cost_probe -- <window-title-substring>
//!
//! `read_element_position` was measured at 1297ms per key-down on a Wikipedia
//! article (3365 named elements) and lost half a recording. The plan is to share
//! one traversal instead of three -- but "three walks" is the shape of the code,
//! not necessarily the shape of the cost, and optimising the wrong one would be
//! effort spent on a guess.
//!
//! So this times each walk separately, and each property read within it, against
//! a real window.
//!
//! Reads only. The walks below deliberately MIRROR `capture::grid`'s rather than
//! calling them, because those are private. They are copies and could drift;
//! this is a measuring instrument, not a second implementation to rely on.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use terminator::{Desktop, UIElement};

#[cfg(windows)]
fn make_dpi_aware() {
    use windows_sys::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Walk reading only `name()`, the shape `collect_position` and
/// `collect_page_identity` share.
fn walk_name(el: &UIElement, depth: usize, budget: &mut usize, n: &mut usize) {
    if *budget == 0 || depth > 14 {
        return;
    }
    *budget -= 1;
    *n += 1;
    let _ = el.name().unwrap_or_default();
    if let Ok(children) = el.children() {
        for c in &children {
            walk_name(c, depth + 1, budget, n);
        }
    }
}

/// Walk reading `id()`, `role()` and `name()`, which is `collect_nodes`.
fn walk_full(el: &UIElement, budget: &mut usize, n: &mut usize) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    *n += 1;
    let _ = el.id().unwrap_or_default();
    let _ = el.role();
    let _ = el.name().unwrap_or_default();
    if let Ok(children) = el.children() {
        for c in &children {
            walk_full(c, budget, n);
        }
    }
}

/// Walk reading `role()` and `name()` but NOT `id()`.
fn walk_no_id(el: &UIElement, budget: &mut usize, n: &mut usize) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    *n += 1;
    let _ = el.role();
    let _ = el.name().unwrap_or_default();
    if let Ok(children) = el.children() {
        for c in &children {
            walk_no_id(c, budget, n);
        }
    }
}

/// The structure alone -- children only, no property reads. The floor.
fn walk_bare(el: &UIElement, budget: &mut usize, n: &mut usize) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    *n += 1;
    if let Ok(children) = el.children() {
        for c in &children {
            walk_bare(c, budget, n);
        }
    }
}

fn timed(label: &str, f: impl FnOnce() -> usize) -> Duration {
    let started = Instant::now();
    let n = f();
    let took = started.elapsed();
    println!("  {label:<34} {n:>5} nodes  {:>8.0}ms", took.as_secs_f64() * 1000.0);
    took
}

#[tokio::main]
async fn main() -> ExitCode {
    #[cfg(windows)]
    make_dpi_aware();

    let want = std::env::args().nth(1).unwrap_or_default();
    let Ok(desktop) = Desktop::new_default() else {
        eprintln!("accessibility engine unavailable");
        return ExitCode::FAILURE;
    };
    // Bring the target window forward FIRST, from inside this process.
    //
    // Running a probe from a terminal returns focus to the terminal, and the
    // first run of this measured the terminal's own 228-node tree while
    // reporting it as the page under test. `Locator::all()` is not an escape:
    // it refuses a desktop-wide selector and demands a `process:` prefix.
    #[cfg(windows)]
    if !focus_window_titled(&want) {
        eprintln!("no window whose title contains {want:?}");
        return ExitCode::FAILURE;
    }
    tokio::time::sleep(Duration::from_millis(900)).await;

    let Ok(anchor) = desktop
        .locator("role:Document")
        .first(Some(Duration::from_secs(15)))
        .await
    else {
        eprintln!("no Document element found");
        return ExitCode::FAILURE;
    };
    let mut root = anchor.clone();
    for _ in 0..12 {
        match root.parent() {
            Ok(Some(parent)) => {
                let reached = parent.role() == "Window";
                root = parent;
                if reached {
                    break;
                }
            }
            _ => break,
        }
    }
    let title = root.name().unwrap_or_default();
    if !title.to_lowercase().contains(&want.to_lowercase()) {
        eprintln!("resolved window is {title:?}, not {want:?} -- refusing to report it as such");
        return ExitCode::FAILURE;
    }
    println!("window: {title:?}\n");
    println!("== each walk, 3000-node budget, as capture::grid runs them ==");

    let mut b = 3000usize;
    let bare = timed("structure only (children)", || {
        let mut n = 0;
        walk_bare(&root, &mut b, &mut n);
        n
    });
    let mut b = 3000usize;
    let name_only = timed("+ name()  (x2: position, page id)", || {
        let mut n = 0;
        walk_name(&root, 0, &mut b, &mut n);
        n
    });
    let mut b = 3000usize;
    let no_id = timed("+ role() + name()  (no id)", || {
        let mut n = 0;
        walk_no_id(&root, &mut b, &mut n);
        n
    });
    let mut b = 3000usize;
    let full = timed("+ id() + role() + name()  (nodes)", || {
        let mut n = 0;
        walk_full(&root, &mut b, &mut n);
        n
    });

    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!("\n== what that says ==");
    println!(
        "  today, three walks : {:.0}ms  (name walk x2 + full walk)",
        2.0 * ms(name_only) + ms(full)
    );
    println!("  merged into one    : {:.0}ms  (the full walk alone)", ms(full));
    println!(
        "  saving from merging: {:.0}ms  ({:.0}%)",
        2.0 * ms(name_only),
        100.0 * 2.0 * ms(name_only) / (2.0 * ms(name_only) + ms(full))
    );
    println!(
        "\n  id() costs         : {:.0}ms of the full walk  ({:.0}% of it)",
        ms(full) - ms(no_id),
        100.0 * (ms(full) - ms(no_id)) / ms(full).max(1.0)
    );
    println!("  structural floor   : {:.0}ms -- unavoidable while the tree is walked at all", ms(bare));
    ExitCode::SUCCESS
}

/// Bring the first top-level window whose title contains `want` to the front.
///
/// Exists because the accessibility query is focus-dependent and a probe
/// launched from a terminal leaves the terminal focused.
#[cfg(windows)]
fn focus_window_titled(want: &str) -> bool {
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    type BOOL = i32;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible, SetForegroundWindow,
        ShowWindow, SW_RESTORE,
    };

    static TARGET: Mutex<Option<String>> = Mutex::new(None);
    static FOUND: Mutex<isize> = Mutex::new(0);

    unsafe extern "system" fn visit(hwnd: HWND, _: LPARAM) -> BOOL {
        unsafe {
            if IsWindowVisible(hwnd) == 0 {
                return 1;
            }
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return 1;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
            let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]).to_lowercase();
            let want = TARGET.lock().unwrap().clone().unwrap_or_default();
            if !want.is_empty() && title.contains(&want) {
                *FOUND.lock().unwrap() = hwnd as isize;
                return 0;
            }
            1
        }
    }

    *TARGET.lock().unwrap() = Some(want.to_lowercase());
    *FOUND.lock().unwrap() = 0;
    unsafe {
        EnumWindows(Some(visit), 0);
        let hwnd = *FOUND.lock().unwrap();
        if hwnd == 0 {
            return false;
        }
        ShowWindow(hwnd as HWND, SW_RESTORE);
        SetForegroundWindow(hwnd as HWND);
    }
    true
}
