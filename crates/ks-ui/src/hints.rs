//! Width-aware priority of the normal-mode hint line: `:` `?` `q` always stay, optional hints
//! give way by priority when the terminal is narrow.

use ks_core::Action;

use crate::text::width;

/// The normal-mode hints in display order, with their priority: lower numbers are kept first
/// when the line is too narrow; priority 0 (`:` `?` `q`) is always shown.
pub(crate) static NORMAL_HINTS: &[(Action, &str)] = &[
    (Action::DirParent, "parent"),
    (Action::DirEnter, "open"),
    (Action::MarkToggle, "mark"),
    (Action::HiddenToggle, "hidden"),
    (Action::GoDrives, "drives"),
    (Action::FilterOpen, "filter"),
    (Action::FindFile, "find"),
    (Action::SearchContents, "search"),
    (Action::PaletteOpen, "commands"),
    (Action::HelpOpen, "help"),
    (Action::Quit, "quit"),
];

/// Priority of a normal-mode hint (0 = never dropped).
fn normal_priority(action: Action) -> u8 {
    match action {
        Action::PaletteOpen | Action::HelpOpen | Action::Quit => 0,
        Action::DirEnter => 1,
        Action::DirParent => 2,
        Action::FilterOpen => 3,
        Action::FindFile => 4,
        Action::MarkToggle => 5,
        Action::SearchContents => 6,
        Action::HiddenToggle => 7,
        _ => 8,
    }
}

/// Cells one hint takes: gap, `‹key›`, space and label.
fn hint_cost(key: &str, label: &str) -> usize {
    2 + width(key) + width(label) + 3
}

/// Drops the lowest-priority hints until the line fits `budget`. Order is preserved.
pub(crate) fn fit_by_priority(
    entries: Vec<(Action, String, String)>,
    budget: usize,
) -> Vec<(String, String)> {
    let cost = |e: &(Action, String, String)| hint_cost(&e.1, &e.2);
    // The first entry has no leading gap.
    let mut used: usize = entries
        .iter()
        .filter(|e| normal_priority(e.0) == 0)
        .map(cost)
        .sum();
    let mut keep: Vec<bool> = entries.iter().map(|e| normal_priority(e.0) == 0).collect();
    let mut by_priority: Vec<usize> = (0..entries.len()).filter(|&i| !keep[i]).collect();
    by_priority.sort_by_key(|&i| normal_priority(entries[i].0));
    for i in by_priority {
        let c = cost(&entries[i]);
        if used + c <= budget + 2 {
            used += c;
            keep[i] = true;
        }
    }
    entries
        .into_iter()
        .zip(keep)
        .filter_map(|((_, key, label), keep)| keep.then_some((key, label)))
        .collect()
}
