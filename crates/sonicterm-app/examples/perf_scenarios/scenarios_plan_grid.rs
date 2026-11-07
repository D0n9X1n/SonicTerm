//! The setup that builds S13's wide layouts: an equal grid of `columns` by `rows` panes, built
//! only from splits and directional focus moves, ending with focus on role 1, the typing pane.
//! Each split halves the focused pane, so a line is built by always halving its largest pane;
//! counts are powers of two, so every pane ends the same size.

use super::SetupAction;
use sonicterm_cfg::keymap::Direction;

/// Panes along one line as they are built: each pane's role and its size in units of the line.
struct Line {
    /// (role, size), in screen order.
    panes: Vec<(usize, u32)>,
    /// The index of the focused pane.
    focus: usize,
}

/// Halve `line` until it holds `count` equal panes, splitting toward `forward` and stepping
/// `back`/`forward` between them; each split opens the role after `next_role` and returns the
/// next unused role.
fn halve(
    line: &mut Line,
    count: usize,
    split: SetupAction,
    (back, forward): (Direction, Direction),
    mut next_role: usize,
    actions: &mut Vec<SetupAction>,
) -> usize {
    while line.panes.len() < count {
        let largest = line.panes.iter().map(|(_, size)| *size).max().unwrap_or(1);
        let target = line.panes.iter().rposition(|(_, size)| *size == largest).unwrap_or(0);
        let (steps, direction) = if target < line.focus {
            (line.focus - target, back)
        } else {
            (target - line.focus, forward)
        };
        actions.extend(std::iter::repeat_n(SetupAction::FocusPane(direction), steps));
        actions.push(split);
        let half = line.panes[target].1 / 2;
        line.panes[target].1 = half;
        line.panes.insert(target + 1, (next_role, half));
        line.focus = target + 1;
        next_role += 1;
    }
    next_role
}

/// The setup for an equal `columns` x `rows` grid (each a power of two) and where every role
/// ends, as (column, row) indexed by role. Role 0 stays top left.
pub(crate) fn grid_setup(columns: usize, rows: usize) -> (Vec<SetupAction>, Vec<(usize, usize)>) {
    let mut actions = Vec::new();
    let unit = |count: usize| u32::try_from(count).expect("a small grid");
    let mut across = Line { panes: vec![(0, unit(columns))], focus: 0 };
    let horizontal = (Direction::Left, Direction::Right);
    let mut next_role =
        halve(&mut across, columns, SetupAction::SplitRight, horizontal, 1, &mut actions);
    // Back to the leftmost column, then fill each column top to bottom, moving right between them:
    // the next column is still one pane, so the move lands on it whatever row focus is on.
    actions.extend(std::iter::repeat_n(SetupAction::FocusPane(Direction::Left), across.focus));
    let mut places = vec![(0, 0); columns * rows];
    let mut row = 0;
    for (column, (role, _)) in across.panes.iter().enumerate() {
        if column > 0 {
            actions.push(SetupAction::FocusPane(Direction::Right));
        }
        let mut down = Line { panes: vec![(*role, unit(rows))], focus: 0 };
        let vertical = (Direction::Up, Direction::Down);
        next_role =
            halve(&mut down, rows, SetupAction::SplitDown, vertical, next_role, &mut actions);
        for (index, (role, _)) in down.panes.iter().enumerate() {
            places[*role] = (column, index);
        }
        row = down.focus;
    }
    // Finish on role 1: in an equal grid each move steps exactly one cell.
    let (target_column, target_row) = places[1];
    let column = columns - 1;
    actions.extend(std::iter::repeat_n(
        SetupAction::FocusPane(Direction::Left),
        column - target_column,
    ));
    let (steps, direction) = if target_row < row {
        (row - target_row, Direction::Up)
    } else {
        (target_row - row, Direction::Down)
    };
    actions.extend(std::iter::repeat_n(SetupAction::FocusPane(direction), steps));
    (actions, places)
}

#[cfg(test)]
#[path = "scenarios_plan_grid_tests.rs"]
mod scenarios_plan_grid_tests;
