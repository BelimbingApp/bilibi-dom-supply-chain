//! What both pages' lists share: the FlexTable each list is drawn with,
//! the built-in columns it hosts, the in-memory sort over the facade's
//! bounded rows, and the two-line cell.
use super::render;
use askama::Template;
use bilibi_database::Result;
use bilibi_ui::{
    BuiltinColumn, EmptyState, FlexRow, FlexTable, Html, ListColumns, ListState, SortDir,
    TableEmpty,
};
use std::cmp::Ordering;

#[derive(Template)]
#[template(path = "two_line_cell.html")]
pub(super) struct TwoLineCell<'a> {
    pub(super) first: &'a str,
    pub(super) second: Option<&'a str>,
}

/// Sortable built-in columns; `right` names the right-aligned ones.
pub(super) fn columns(columns: &[(&str, &str)], right: &[&str]) -> Vec<BuiltinColumn> {
    columns
        .iter()
        .map(|(id, label)| {
            let column = BuiltinColumn::new(id, label).sortable(id);
            if right.contains(id) {
                column.right()
            } else {
                column
            }
        })
        .collect()
}

/// Orders `rows` by the list's sort column and direction; ties keep the
/// facade's order.
pub(super) fn sort<T>(
    rows: &mut [T],
    list: &ListState,
    compare: impl Fn(&T, &T, &str) -> Ordering,
) {
    let column = list.sort_by.as_deref().unwrap_or_default();
    let descending = list.sort_dir == Some(SortDir::Desc);
    rows.sort_by(|a, b| {
        let order = compare(a, b, column);
        if descending {
            order.reverse()
        } else {
            order
        }
    });
}

#[allow(clippy::too_many_arguments)] // one call per table, each naming every part
pub(super) fn table(
    id: &str,
    caption: &str,
    event: &str,
    columns: &ListColumns,
    list: &ListState,
    rows: Vec<FlexRow>,
    actions: bool,
    empty: EmptyState,
) -> Result<Html> {
    let none = rows.is_empty();
    let mut table = FlexTable::new(id, columns.flex_columns(), rows)
        .sorted(list.sort_by.as_deref(), list.sort_dir);
    table.event = event;
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    table.caption = caption;
    table.framed = false;
    table.actions = actions;
    if none {
        table.empty = Some(TableEmpty::State(empty));
    }
    render(&table)
}
