//! Is this window a usable test surface for the row-pitch prediction?
//!
//!     cargo run --example pitch_candidate_probe -- <window-title-substring>
//!
//! Two things have to hold, and BOTH have to be measured rather than eyeballed.
//! Tonight produced two wrong page choices in a row -- a pricing page that
//! looked horizontal and rendered vertical, and a Wikipedia table that was real
//! but too large to record -- and in both cases the tree would have said so
//! first.
//!
//! 1. **Small enough that the 400ms walk cap does not bite.** Reported as the
//!    real walk time, not a node count, because
//!    `capture::grid::WALK_TIME_BUDGET_MS` bounds time.
//! 2. **A dense repeating list**, pitch below
//!    `detect::candidates::RECORD_PITCH_FLOOR_PX`, so `record_pitch` has a real
//!    chance to decline as predicted.
//!
//! Reads only. The pitch detector below MIRRORS the one in `detect::candidates`
//! -- that one is private -- and uses its public constants so the two cannot
//! disagree about the thresholds, only about the code. It is an instrument, not
//! a second implementation.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use paradigm_lib::detect::candidates::RECORD_PITCH_TOLERANCE_PX;

/// The magnitude floor as it stood before 2026-08-23, kept LOCAL to this probe
/// so the comparison against the old behaviour survives its removal from the
/// library.
const RECORD_PITCH_FLOOR_PX: f64 = 120.0;
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

/// The full walk `capture::grid::collect_nodes` performs, timed.
fn walk(el: &UIElement, budget: &mut usize, out: &mut Vec<(String, f64, f64, f64)>) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    let _ = el.id().unwrap_or_default();
    let _ = el.role();
    let name = el.name().unwrap_or_default();
    if !name.trim().is_empty() {
        if let Ok((x, y, w, _h)) = el.bounds() {
            out.push((name, x, y, w));
        }
    }
    if let Ok(children) = el.children() {
        for c in &children {
            walk(c, budget, out);
        }
    }
}

/// `detect::candidates::record_pitch`, mirrored, with `floor` variable so the
/// same data can be scored against the shipped floor and against none.
fn record_pitch(ys: &[f64], floor: f64) -> Option<f64> {
    let mut diffs: Vec<f64> = Vec::new();
    for (i, a) in ys.iter().enumerate() {
        for b in ys.iter().skip(i + 1) {
            let d = (b - a).abs();
            if d >= floor {
                diffs.push(d);
            }
        }
    }
    diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut clusters: Vec<Vec<f64>> = Vec::new();
    for d in diffs {
        match clusters.last_mut() {
            Some(c) if d - c[c.len() - 1] <= RECORD_PITCH_TOLERANCE_PX => c.push(d),
            _ => clusters.push(vec![d]),
        }
    }
    let best = clusters.iter().filter(|c| c.len() >= 2).map(|c| c.len()).max()?;
    clusters
        .iter()
        .filter(|c| c.len() == best)
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .min_by(|a, b| a.partial_cmp(b).unwrap())
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
    #[cfg(windows)]
    if !focus_window_titled(&want) {
        eprintln!("no window whose title contains {want:?}");
        return ExitCode::FAILURE;
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // The FOCUSED element, walked up to its window -- exactly what
    // `capture::grid::read_position` anchors on, so this measures the tree
    // production would actually walk. A `role:Document` anchor was tried first
    // and cannot see a native app: File Explorer has no Document at all.
    // Focus first -- it is what read_position anchors on, and it is the only
    // thing that reaches a native app. Browsers sometimes walk up from the
    // focused element to an unnamed ancestor, so fall back to the Document.
    let anchor = match desktop.focused_element() {
        Ok(el) => el,
        Err(_) => match desktop
            .locator("role:Document")
            .first(Some(Duration::from_secs(10)))
            .await
        {
            Ok(el) => el,
            Err(_) => {
                eprintln!("no focused element and no Document");
                return ExitCode::FAILURE;
            }
        },
    };
    let mut root = anchor.clone();
    #[allow(unused_assignments)]
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
    let mut title = root.name().unwrap_or_default();
    if !title.to_lowercase().contains(&want.to_lowercase()) {
        // Second chance through the Document anchor, for pages whose focused
        // element walks up to an unnamed ancestor -- Gmail does this.
        if let Ok(doc) = desktop
            .locator("role:Document")
            .first(Some(Duration::from_secs(10)))
            .await
        {
            let mut up = doc;
            for _ in 0..12 {
                match up.parent() {
                    Ok(Some(p)) => {
                        let reached = p.role() == "Window";
                        up = p;
                        if reached {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            title = up.name().unwrap_or_default();
            root = up;
        }
    }
    if !title.to_lowercase().contains(&want.to_lowercase()) {
        eprintln!("resolved window is {title:?}, not {want:?} -- refusing to report it as such");
        return ExitCode::FAILURE;
    }

    println!("== {title} ==");

    let mut budget = 3000usize;
    let mut els = Vec::new();
    let started = Instant::now();
    walk(&root, &mut budget, &mut els);
    let took = started.elapsed().as_millis();
    let visited = 3000 - budget;

    println!("\n1. TREE SIZE  (needs to finish inside the 400ms walk cap)");
    println!("   nodes visited        {visited}{}", if budget == 0 { "  (BUDGET SPENT -- tree is larger)" } else { "" });
    println!("   named, with bounds   {}", els.len());
    println!("   full walk            {took}ms");
    println!(
        "   verdict              {}",
        if took < 400 {
            "PASS -- comfortably inside the cap"
        } else if took < 600 {
            "MARGINAL -- near the cap, results would be non-deterministic"
        } else {
            "FAIL -- the cap would decline most reads here"
        }
    );

    // The densest column is the best proxy for a repeating list: a list's items
    // share an x and step down in y.
    let mut by_x: std::collections::BTreeMap<i64, Vec<f64>> = Default::default();
    for (_, x, y, _) in &els {
        by_x.entry(*x as i64).or_default().push(*y);
    }
    let Some((x, ys)) = by_x
        .into_iter()
        .map(|(x, mut ys)| {
            ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
            ys.dedup();
            (x, ys)
        })
        .max_by_key(|(_, ys)| ys.len())
    else {
        println!("\n2. no elements with bounds at all");
        return ExitCode::SUCCESS;
    };

    println!("\n2. DENSEST REPEATING COLUMN  (x={x}, {} distinct rows)", ys.len());
    println!("   row y values         {:?}", ys.iter().map(|y| *y as i64).take(12).collect::<Vec<_>>());
    let gaps: Vec<i64> = ys.windows(2).map(|w| (w[1] - w[0]) as i64).collect();
    println!("   consecutive gaps     {:?}", gaps.iter().take(14).collect::<Vec<_>>());
    let true_pitch = record_pitch(&ys, 1.0);
    println!(
        "   pitch with NO floor  {}",
        true_pitch.map(|p| format!("{p:.1}px")).unwrap_or_else(|| "none -- not a repeating list".into())
    );
    let shipped = record_pitch(&ys, RECORD_PITCH_FLOOR_PX);
    println!(
        "   pitch at the {RECORD_PITCH_FLOOR_PX:.0}px floor  {}",
        shipped.map(|p| format!("{p:.1}px")).unwrap_or_else(|| "NONE -- record_pitch declines".into())
    );

    println!("\n3. AS A TEST SURFACE");
    match (true_pitch, shipped) {
        (Some(p), None) if p < RECORD_PITCH_FLOOR_PX => println!(
            "   IDEAL. A real repeating list at {p:.0}px, and the shipped floor\n   \
             declines it -- exactly the predicted failure, on real data."
        ),
        (Some(p), Some(s)) => println!(
            "   Repeating at {p:.0}px and the floor still finds {s:.0}px, so this\n   \
             would NOT exercise the prediction. Informative, but not the test."
        ),
        (None, _) => println!("   Not a repeating list. Wrong surface."),
        _ => println!("   Mixed signal -- read the numbers above rather than this line."),
    }
    ExitCode::SUCCESS
}

/// Bring the first top-level window whose title contains `want` to the front.
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
