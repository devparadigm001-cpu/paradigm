//! Phase 1 Step 2: prove Terminator can read the accessibility tree and act on
//! a real window. Capability probe only -- no recording, no logging, no DB.
//!
//!     cargo run --example terminator_probe            # defaults to notepad
//!     cargo run --example terminator_probe -- notepad
//!
//! The probe launches its own target so it does not depend on the user having
//! anything open, and leaves it open at the end so Windows never raises a
//! "save changes?" dialog on our behalf.
//!
//! Every step prints what it EXPECTED and what it actually FOUND, so a failure
//! says which element was missing rather than panicking on an unwrap.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use terminator::{AutomationError, Desktop, UIElement};

/// Depth cap for tree walks. Notepad is shallow; this stops a pathological
/// window from turning the probe into an infinite crawl.
const MAX_DEPTH: usize = 12;
/// Hard cap on visited nodes, for the same reason.
const MAX_NODES: usize = 4000;
/// How long to wait for a freshly launched app to show up in the tree.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Outcome of one probe step.
///
/// `Skipped` and `Failed` are deliberately distinct: "I declined to act because
/// acting would have been unsafe" is not the same result as "I acted and it
/// broke", and collapsing them into one FAILED made a correct safety refusal
/// read like a Terminator defect.
enum StepOutcome {
    Passed,
    Skipped(String),
    Failed(String),
}

impl StepOutcome {
    fn label(&self) -> &'static str {
        match self {
            StepOutcome::Passed => "ok",
            StepOutcome::Skipped(_) => "SKIPPED",
            StepOutcome::Failed(_) => "FAILED",
        }
    }

    fn reason(&self) -> Option<&str> {
        match self {
            StepOutcome::Passed => None,
            StepOutcome::Skipped(r) | StepOutcome::Failed(r) => Some(r),
        }
    }
}

/// Roles that can accept typed text. Notepad's editor surfaces differently
/// across Windows versions (classic Edit vs. WinUI Document), so accept both.
///
/// "Text" is deliberately excluded: in UIA it is almost always a static label.
/// Including it made the probe report 14 "editable surfaces" in Notepad, 13 of
/// which were tab titles.
const EDITABLE_ROLES: &[&str] = &["Edit", "Document"];

/// Opt this process into per-monitor DPI awareness, and report the effect.
///
/// This must happen before anything reads screen metrics. UI Automation always
/// reports element bounds in PHYSICAL pixels, but `GetSystemMetrics` is
/// virtualized for a DPI-unaware process -- on a scaled primary display it
/// returns logical pixels instead. Terminator's `click_at_coordinates` divides
/// the (physical) target x by `SM_CXSCREEN`, so if that denominator is
/// virtualized the click overshoots and lands nowhere near the element.
///
/// Rust binaries ship no DPI manifest and are therefore unaware by default,
/// which is why this call, not a custom mouse implementation, is the fix.
#[cfg(windows)]
fn make_dpi_aware() {
    use windows_sys::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    let before = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let ok = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let after = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };

    println!(
        "[{}] per-monitor DPI awareness: primary metrics {}x{} -> {}x{}",
        if ok != 0 { "ok" } else { ".." },
        before.0,
        before.1,
        after.0,
        after.1
    );
    if before != after {
        println!(
            "     screen metrics were virtualized before this call; coordinate clicks \
             would have overshot"
        );
    }
}

fn main() -> ExitCode {
    // Before any screen metric is read, including by Terminator.
    #[cfg(windows)]
    make_dpi_aware();

    let app_name = std::env::args().nth(1).unwrap_or_else(|| "notepad".to_string());

    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(probe(&app_name)),
        Err(e) => {
            eprintln!("FAIL: could not start tokio runtime: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn probe(app_name: &str) -> ExitCode {
    println!("== Terminator capability probe ==");
    println!("target application: {app_name}\n");

    let desktop = match Desktop::new_default() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: could not initialise the accessibility engine.");
            eprintln!("  expected: a UI Automation session on an interactive desktop");
            eprintln!("  found:    {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("[ok] accessibility engine initialised");

    let app = match acquire_app(&desktop, app_name) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("\nFAIL: could not obtain a window for {app_name:?}.");
            eprintln!("  expected: {app_name:?} running and visible in the UI tree");
            eprintln!("  found:    {e}");
            eprintln!("  hint:     open {app_name:?} manually, then re-run this probe.");
            report_visible_apps(&desktop);
            return ExitCode::FAILURE;
        }
    };

    // Foreground the window before reading: some frameworks (WinUI in
    // particular) only fully populate their UIA tree once activated.
    if let Err(e) = app.activate_window() {
        println!("[..] could not activate window, continuing: {e}");
    }
    std::thread::sleep(Duration::from_millis(800));

    let attrs = app.attributes();
    println!(
        "[ok] attached to window: role={} name={:?}\n",
        attrs.role,
        attrs.name.as_deref().unwrap_or("<unnamed>")
    );

    // ---- (a) read the tree -------------------------------------------------
    let tree = walk(&app);
    print_tree_summary(&tree);

    // ---- (b) one real click, proven by an observable state change ----------
    let click_ok = prove_click(&desktop, &app, &tree).await;

    // ---- (c) one real type action, proven by reading the value back --------
    let type_ok = prove_type(&app);

    println!("\n== result ==");
    println!("  read  : ok ({} nodes)", tree.nodes.len());
    for (name, outcome) in [("click", &click_ok), ("type ", &type_ok)] {
        match outcome.reason() {
            Some(reason) => println!("  {name} : {} -- {reason}", outcome.label()),
            None => println!("  {name} : {}", outcome.label()),
        }
    }

    let failed = [&click_ok, &type_ok]
        .iter()
        .any(|o| matches!(o, StepOutcome::Failed(_)));
    let skipped = [&click_ok, &type_ok]
        .iter()
        .any(|o| matches!(o, StepOutcome::Skipped(_)));

    if failed {
        eprintln!("\nFAIL: a step errored. See the expected-vs-found detail above.");
        ExitCode::from(1)
    } else if skipped {
        // Not a failure and not a pass: nothing broke, but capability is
        // unproven, so the probe must not claim success.
        println!("\nINCOMPLETE: nothing failed, but a step declined to run.");
        println!("Capability is unproven until every step actually executes.");
        ExitCode::from(2)
    } else {
        println!("\nPASS: Terminator can read, click, and type against a real window.");
        println!(
            "note: {app_name} was left open. The probe text is in a scratch tab it \
             created itself; close that tab without saving. No pre-existing document \
             was written to."
        );
        ExitCode::SUCCESS
    }
}

/// A window whose tree is just the frame and its title bar is not realized --
/// `application()` can return a stale or hidden match for a process that is not
/// actually running. Acting on one of those looks like "Terminator is broken"
/// when the real problem is that we attached to nothing.
fn looks_realized(el: &UIElement) -> bool {
    walk(el).nodes.len() > 2
}

/// Attach to a realized running instance, else launch one and wait for it.
fn acquire_app(desktop: &Desktop, app_name: &str) -> Result<UIElement, AutomationError> {
    if let Ok(app) = desktop.application(app_name) {
        if looks_realized(&app) {
            println!("[ok] found {app_name:?} already running");
            return Ok(app);
        }
        println!(
            "[..] {app_name:?} matched an unrealized window (name={:?}, {} nodes); \
             launching a real one instead",
            app.name().unwrap_or_else(|| "<unnamed>".into()),
            walk(&app).nodes.len()
        );
    }

    println!("[..] launching {app_name:?}");
    desktop.open_application(app_name)?;

    // open_application returns before the window is necessarily in the tree,
    // and the tree is not populated the instant the window appears.
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    let mut last_seen = None;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        if let Ok(app) = desktop.application(app_name) {
            if looks_realized(&app) {
                println!("[ok] launched {app_name:?}");
                return Ok(app);
            }
            last_seen = Some(walk(&app).nodes.len());
        }
    }

    Err(AutomationError::Timeout(format!(
        "{app_name:?} did not produce a realized window within {LAUNCH_TIMEOUT:?} \
         (best seen: {} nodes, need >2)",
        last_seen.map(|n| n.to_string()).unwrap_or_else(|| "no match".into())
    )))
}

/// On failure, show what IS on screen so the mismatch is diagnosable.
fn report_visible_apps(desktop: &Desktop) {
    match desktop.applications() {
        Ok(apps) if !apps.is_empty() => {
            eprintln!("  visible applications right now:");
            for a in apps.iter().take(15) {
                eprintln!(
                    "    - {:?} (role={})",
                    a.name().unwrap_or_else(|| "<unnamed>".into()),
                    a.role()
                );
            }
        }
        Ok(_) => eprintln!("  visible applications right now: none reported"),
        Err(e) => eprintln!("  (could not enumerate applications either: {e})"),
    }
}

struct Node {
    depth: usize,
    role: String,
    name: Option<String>,
    value: Option<String>,
    enabled: Option<bool>,
    focusable: Option<bool>,
    element: UIElement,
}

struct Tree {
    nodes: Vec<Node>,
    truncated: bool,
}

fn walk(root: &UIElement) -> Tree {
    let mut nodes = Vec::new();
    let mut truncated = false;
    walk_into(root, 0, &mut nodes, &mut truncated);
    Tree { nodes, truncated }
}

fn walk_into(el: &UIElement, depth: usize, out: &mut Vec<Node>, truncated: &mut bool) {
    if out.len() >= MAX_NODES {
        *truncated = true;
        return;
    }

    let a = el.attributes();
    out.push(Node {
        depth,
        role: a.role.clone(),
        name: a.name.clone(),
        value: a.value.clone(),
        enabled: a.enabled,
        focusable: a.is_keyboard_focusable,
        element: el.clone(),
    });

    if depth >= MAX_DEPTH {
        *truncated = true;
        return;
    }

    // A subtree we cannot enumerate is normal (permission, detached element);
    // it must not abort the whole walk.
    if let Ok(children) = el.children() {
        for child in &children {
            walk_into(child, depth + 1, out, truncated);
        }
    }
}

fn print_tree_summary(tree: &Tree) {
    println!("-- accessibility tree summary --");
    println!("visited {} nodes{}", tree.nodes.len(), if tree.truncated { " (truncated)" } else { "" });

    let mut by_role: BTreeMap<&str, usize> = BTreeMap::new();
    for n in &tree.nodes {
        *by_role.entry(n.role.as_str()).or_insert(0) += 1;
    }
    println!("\ncontrol types seen:");
    for (role, count) in &by_role {
        println!("  {count:>4}  {role}");
    }

    println!("\nnamed controls (first 30):");
    let mut shown = 0;
    for n in &tree.nodes {
        let Some(name) = n.name.as_deref().filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        let indent = "  ".repeat(n.depth.min(8));
        let mut flags = Vec::new();
        if n.enabled == Some(false) {
            flags.push("disabled");
        }
        if n.focusable == Some(true) {
            flags.push("focusable");
        }
        let flag_str = if flags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", flags.join(", "))
        };
        println!("  {indent}{:<14} {name:?}{flag_str}", n.role);
        shown += 1;
        if shown >= 30 {
            break;
        }
    }

    let editable: Vec<&Node> = tree.nodes.iter().filter(|n| is_editable(n)).collect();
    println!("\neditable surfaces found: {}", editable.len());
    for n in editable.iter().take(5) {
        println!(
            "  {:<14} name={:?} value={:?}",
            n.role,
            n.name.as_deref().unwrap_or("<unnamed>"),
            n.value.as_deref().unwrap_or("<none>")
        );
    }
}

fn is_editable(n: &Node) -> bool {
    EDITABLE_ROLES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(&n.role))
}

fn count_role(root: &UIElement, role: &str) -> usize {
    walk(root)
        .nodes
        .iter()
        .filter(|n| n.role.eq_ignore_ascii_case(role))
        .count()
}

/// Click proof: press "Add New Tab" and watch a TabItem appear. The click is
/// proven by a state change it caused, not by click() returning Ok.
///
/// This target is chosen deliberately: it also yields an EMPTY scratch tab, so
/// the typing step below never touches a document the user had open. The
/// obvious alternative -- opening the File menu -- fails on Windows 11 Notepad
/// anyway, because that MenuItem is present in the tree but not visible
/// ("Element is not visible") until its parent menu is expanded.
async fn prove_click(desktop: &Desktop, app: &UIElement, tree: &Tree) -> StepOutcome {
    println!("\n-- click --");

    let before = count_role(app, "TabItem");
    println!("TabItem elements before click: {before}");

    // Selector-based lookup, the way the real pipeline will find things.
    let target = match app.locator("role:Button|name:Add New Tab") {
        Ok(loc) => loc.first(Some(Duration::from_secs(5))).await.ok(),
        Err(e) => {
            println!("  (locator construction failed: {e})");
            None
        }
    };

    // Deliberately NOT filtered on is_visible(): see the defect note below --
    // is_visible() is unreliable on a secondary monitor, so filtering on it
    // would discard a perfectly clickable element.
    let target = target.or_else(|| {
        tree.nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("Add New Tab"))
            .map(|n| n.element.clone())
    });

    let Some(button) = target else {
        eprintln!("  expected: a control named \"Add New Tab\"");
        eprintln!("  found:    no such control in {} visited nodes", tree.nodes.len());
        eprintln!("  the named controls listed above are everything the probe could see.");
        return StepOutcome::Failed("no \"Add New Tab\" control found".into());
    };

    println!(
        "target: role={} name={:?}",
        button.role(),
        button.name().unwrap_or_else(|| "<unnamed>".into())
    );

    // Confirm the element really does have a usable rect. is_visible() can
    // return false either because bounds are zero-sized OR because of the
    // work-area defect below; printing the rect distinguishes them.
    let bounds = match button.bounds() {
        Ok(b) => {
            println!("bounds: x={:.0} y={:.0} w={:.0} h={:.0}", b.0, b.1, b.2, b.3);
            if b.2 <= 0.0 || b.3 <= 0.0 {
                eprintln!("  expected: non-zero width/height");
                eprintln!("  found:    zero-sized rect, so this element really is not clickable");
                return StepOutcome::Failed("element has zero-sized bounds".into());
            }
            println!("[ok] bounds are non-zero, so the element has a real on-screen rect");
            b
        }
        Err(e) => {
            eprintln!("  expected: bounds() to return the element rect");
            eprintln!("  found:    {e}");
            return StepOutcome::Failed(format!("bounds() failed: {e}"));
        }
    };

    let reported_visible = button.is_visible();
    println!("is_visible(): {reported_visible:?}");

    // Try the normal path first, so we only take the workaround when needed.
    let mut used_workaround = false;
    let click_result = button.click().map(|_| ());
    let click_result = match click_result {
        Ok(()) => Ok(()),
        Err(AutomationError::ElementNotVisible(ref msg)) => {
            print_multi_monitor_defect_note(desktop, bounds, msg).await;
            used_workaround = true;
            let (cx, cy) = (bounds.0 + bounds.2 / 2.0, bounds.1 + bounds.3 / 2.0);
            println!("compensating: real mouse click at screen coordinates ({cx:.0}, {cy:.0})");
            desktop.click_at_coordinates(cx, cy)
        }
        Err(e) => Err(e),
    };

    if let Err(e) = click_result {
        eprintln!("  expected: a click to reach \"Add New Tab\"");
        eprintln!("  found:    {e}");
        return StepOutcome::Failed(format!("click failed even at raw coordinates: {e}"));
    }
    if !used_workaround {
        println!("clicked via element.click() (no workaround needed)");
    }

    std::thread::sleep(Duration::from_millis(900));
    let after = count_role(app, "TabItem");
    println!("TabItem elements after click:  {after}");

    if after > before {
        println!("[ok] click caused {} new TabItem element(s) to appear", after - before);
        if used_workaround {
            println!("     (via the coordinate-click workaround, not element.click())");
        }
        StepOutcome::Passed
    } else {
        eprintln!("  expected: TabItem count to rise after clicking \"Add New Tab\"");
        eprintln!("  found:    {before} -> {after} (no observable change)");
        StepOutcome::Failed(format!(
            "click produced no observable change (TabItem {before} -> {after})"
        ))
    }
}

/// Say out loud that we are compensating for a known upstream defect, and show
/// the numbers that prove it. A silent workaround would let this bug reach
/// Phase 2 disguised as working code.
async fn print_multi_monitor_defect_note(
    desktop: &Desktop,
    bounds: (f64, f64, f64, f64),
    msg: &str,
) {
    println!("\n  !! element.click() refused: \"{msg}\"");
    println!("  !! KNOWN DEFECT in terminator-rs 0.23.35 (multi-monitor).");
    println!("  !! UIElement::is_visible() tests the element rect against");
    println!("  !! WorkArea::get_primary(), which is SystemParametersInfoW(SPI_GETWORKAREA)");
    println!("  !! -- the PRIMARY monitor's work area only. Any element on a secondary");
    println!("  !! monitor fails that test and every click on it is rejected.");

    match desktop.get_primary_monitor().await {
        Ok(m) => {
            let (wx, wy, ww, wh) = match m.work_area {
                Some(w) => (w.x, w.y, w.width as i32, w.height as i32),
                None => (m.x, m.y, m.width as i32, m.height as i32),
            };
            println!("  !!   primary work area : x={wx} y={wy} w={ww} h={wh} (right edge {})", wx + ww);
            println!(
                "  !!   this element      : x={:.0} y={:.0} w={:.0} h={:.0} (left edge {:.0})",
                bounds.0, bounds.1, bounds.2, bounds.3, bounds.0
            );
            if bounds.0 >= (wx + ww) as f64 {
                println!(
                    "  !!   {:.0} >= {}, so intersects() is false and is_visible() returns false",
                    bounds.0,
                    wx + ww
                );
            }
        }
        Err(e) => println!("  !!   (could not read primary monitor bounds: {e})"),
    }

    println!("  !! Correct upstream fix: resolve the work area of the monitor UNDER the");
    println!("  !! element (MonitorFromRect + GetMonitorInfo), not the primary monitor.");
    println!("  !! Documented in docs/known-issues/terminator-multi-monitor-visibility.md");
}

/// Type proof: write a unique marker into an EMPTY editor and read it back.
///
/// Re-walks the tree because the click above created a new tab. The empty
/// check is a hard safety gate, not a nicety: an earlier version of this probe
/// typed into whichever document happened to be focused, and Windows 11
/// Notepad restores the user's previous session on launch -- so it wrote into a
/// real project file's buffer. Refusing any non-empty document makes that
/// impossible rather than unlikely.
fn prove_type(app: &UIElement) -> StepOutcome {
    println!("\n-- type --");

    let marker = format!(
        "paradigm-probe-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );

    let tree = walk(app);
    let candidates: Vec<&Node> = tree.nodes.iter().filter(|n| is_editable(n)).collect();
    if candidates.is_empty() {
        eprintln!("  expected: a control with role in {EDITABLE_ROLES:?}");
        eprintln!("  found:    none among {} visited nodes", tree.nodes.len());
        return StepOutcome::Failed("no editable control found in the window".into());
    }

    let empty = candidates
        .iter()
        .find(|n| read_text(&n.element).trim().is_empty());

    let Some(node) = empty else {
        eprintln!("  expected: an EMPTY editable control to type into");
        eprintln!("  found:    {} editable control(s), all non-empty:", candidates.len());
        for n in candidates.iter().take(5) {
            eprintln!(
                "    role={} name={:?} content={:?}",
                n.role,
                n.name.as_deref().unwrap_or("<unnamed>"),
                truncate(&read_text(&n.element), 40)
            );
        }
        eprintln!("  refusing to type into a document that already has content.");
        return StepOutcome::Skipped(format!(
            "no empty editable control; refused to write into {} document(s) that already have content",
            candidates.len()
        ));
    };

    let editor = &node.element;
    println!(
        "typing into: role={} name={:?} (verified empty)",
        node.role,
        node.name.as_deref().unwrap_or("<unnamed>")
    );

    let before = read_text(editor);
    println!("value before: {:?}", truncate(&before, 60));

    if let Err(e) = editor.focus() {
        println!("  (focus failed, continuing -- type_text focuses internally: {e})");
    }
    // use_clipboard = false: real synthetic keystrokes, not a paste.
    if let Err(e) = editor.type_text(&marker, false) {
        eprintln!("  expected: type_text to deliver {marker:?}");
        eprintln!("  found:    {e}");
        return StepOutcome::Failed(format!("type_text errored: {e}"));
    }
    std::thread::sleep(Duration::from_millis(600));

    let after = read_text(editor);
    println!("value after:  {:?}", truncate(&after, 60));
    println!("typed marker: {marker:?}");

    if after.contains(&marker) {
        println!("[ok] marker read back from the live control");
        StepOutcome::Passed
    } else {
        eprintln!("  expected: the control's value to contain {marker:?}");
        eprintln!("  found:    {:?}", truncate(&after, 200));
        if after == before {
            eprintln!("  the value did not change at all, so the keystrokes never landed.");
        }
        StepOutcome::Failed("marker was not present in the control after typing".into())
    }
}

/// Editors expose their content through ValuePattern or TextPattern depending
/// on the control; try both before concluding nothing was typed.
fn read_text(el: &UIElement) -> String {
    match el.get_value() {
        Ok(Some(v)) if !v.is_empty() => return v,
        Ok(_) => {}
        Err(e) => println!("  (get_value unavailable: {e})"),
    }
    el.text(3).unwrap_or_default()
}

fn truncate(s: &str, max: usize) -> String {
    let cleaned = s.replace(['\r', '\n'], "\\n");
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let head: String = cleaned.chars().take(max).collect();
        format!("{head}...")
    }
}
