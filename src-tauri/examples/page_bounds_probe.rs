//! Dump every named element's bounds from the front window.
//!
//!     cargo run --example page_bounds_probe -- <title-substring>
//!
//! Exists because the record/field clustering is validated on ONE layout --
//! OrderFlow's vertical card list -- and a claim about geometry needs more than
//! one geometry behind it. This reads bounds directly from a rendered page, so
//! a layout can be tested without a Record Mode session.
//!
//! Reads only. Clicks nothing, types nothing.

use std::process::ExitCode;
use std::time::Duration;

use terminator::{Desktop, UIElement};

fn walk(el: &UIElement, depth: usize, budget: &mut usize, out: &mut Vec<(String, String, (f64, f64, f64, f64))>) {
    if *budget == 0 || depth > 30 {
        return;
    }
    *budget -= 1;
    let name = el.name().unwrap_or_default();
    if !name.trim().is_empty() {
        if let Ok(b) = el.bounds() {
            out.push((el.role(), name, b));
        }
    }
    if let Ok(children) = el.children() {
        for c in &children {
            walk(c, depth + 1, budget, out);
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let want = std::env::args().nth(2).unwrap_or_default();

    let Ok(desktop) = Desktop::new_default() else {
        eprintln!("accessibility engine unavailable");
        return ExitCode::FAILURE;
    };
    let Ok(anchor) = desktop
        .locator("role:Document")
        .first(Some(Duration::from_secs(15)))
        .await
    else {
        eprintln!("no Document element found");
        return ExitCode::FAILURE;
    };

    let mut window = anchor.clone();
    for _ in 0..40 {
        match window.parent() {
            Ok(Some(p)) => {
                let is_window = p.attributes().role == "Window";
                window = p;
                if is_window {
                    break;
                }
            }
            _ => break,
        }
    }
    let title = window.attributes().name.unwrap_or_default();
    println!("window: {title:?}");
    if !want.is_empty() && !title.contains(&want) {
        eprintln!("front window does not match {want:?}. Aborting rather than reporting on whatever else is in front.");
        return ExitCode::FAILURE;
    }

    let mut out = Vec::new();
    let mut budget = 4000usize;
    walk(&window, 0, &mut budget, &mut out);

    println!("named elements with bounds: {}\n", out.len());
    println!("{:>7} {:>7} {:>7} {:>7}  {:<14} {}", "x", "y", "w", "h", "role", "name");
    for (role, name, (x, y, w, h)) in &out {
        println!(
            "{x:>7.0} {y:>7.0} {w:>7.0} {h:>7.0}  {:<14} {}",
            role.chars().take(14).collect::<String>(),
            name.chars().take(46).collect::<String>()
        );
    }
    ExitCode::SUCCESS
}
