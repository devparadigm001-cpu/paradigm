//! What roles does the Paradigm window actually expose? Dumped, not guessed.
use std::process::ExitCode;
use std::time::Duration;
use terminator::{Desktop, UIElement};

fn walk(el: &UIElement, depth: usize, budget: &mut usize, out: &mut Vec<(String, String, usize)>) {
    if *budget == 0 || depth > 25 {
        return;
    }
    *budget -= 1;
    let role = el.role();
    let name = el.name().unwrap_or_default();
    out.push((role, name, depth));
    if let Ok(children) = el.children() {
        for c in children {
            walk(&c, depth + 1, budget, out);
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let Ok(desktop) = Desktop::new(false, false) else {
        eprintln!("no desktop");
        return ExitCode::FAILURE;
    };
    let windows = desktop
        .locator("role:Window")
        .within(desktop.root())
        .all(Some(Duration::from_secs(8)), Some(3))
        .await
        .unwrap_or_default();
    let Some(window) = windows
        .into_iter()
        .find(|w| w.name().unwrap_or_default().trim() == "Paradigm")
    else {
        eprintln!("no Paradigm window");
        return ExitCode::FAILURE;
    };

    let mut out = Vec::new();
    let mut budget = 4000usize;
    walk(&window, 0, &mut budget, &mut out);

    println!("nodes walked: {} (budget left {budget})", out.len());
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for (role, _, _) in &out {
        *counts.entry(role.clone()).or_default() += 1;
    }
    println!("\n== roles present ==");
    for (role, n) in &counts {
        println!("  {role:<20} {n}");
    }
    println!("\n== anything whose name looks like a control ==");
    for (role, name, depth) in &out {
        let n = name.trim();
        if n.is_empty() {
            continue;
        }
        if n.contains("record")
            || n.contains("Record")
            || n.contains("Refresh")
            || n.contains("Check")
            || n.contains("Preview")
            || n.contains("Run")
            || n.contains("Save") || n.contains("playbook") || n.contains("step") || n.contains("Type into")
        {
            println!("  d{depth:<2} {role:<16} {n:?}");
        }
    }
    ExitCode::SUCCESS
}
