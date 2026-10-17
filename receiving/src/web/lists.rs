//! What the receiving lists share: the FlexTable each list is drawn with,
//! the built-in sortable columns it hosts, the in-memory sort over the
//! facade's bounded rows, the two-line cell and a detail page's grid.
use super::render;
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_ui::{
    list_state::ParamNames, params, BuiltinColumn, ColumnsOutcome, EmptyState, FlexRow, FlexTable,
    Html, ListColumns, ListSpec, ListState, SortDir, TableEmpty,
};
use serde_json::Value;
use std::{cmp::Ordering, collections::HashMap};

/// A list cell with a quieter second line, left out of a compact row by the
/// caller (a reversal's original, a reversed receipt's reversal).
#[derive(Template)]
#[template(path = "two_line_cell.html")]
pub(super) struct TwoLineCell<'a> {
    pub(super) first: Html,
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

pub(super) struct Table<'a> {
    pub(super) id: &'a str,
    pub(super) caption: &'a str,
    pub(super) event: &'a str,
    pub(super) columns: &'a ListColumns,
    pub(super) list: &'a ListState,
}
impl Table<'_> {
    pub(super) fn render(&self, rows: Vec<FlexRow>, empty: EmptyState) -> Result<Html> {
        let none = rows.is_empty();
        let mut table = FlexTable::new(self.id, self.columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.event = self.event;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = self.caption;
        table.framed = false;
        table.actions = false;
        if none {
            table.empty = Some(TableEmpty::State(empty));
        }
        render(&table)
    }
}

/// One FlexTable of a detail page, sorted under its own query names so
/// several tables on one page keep their sort in the address.
pub(super) struct Grid {
    pub(super) columns: ListColumns,
    pub(super) list: ListState,
    id: &'static str,
    event: &'static str,
}
impl Grid {
    pub(super) fn new(
        id: &'static str,
        event: &'static str,
        prefix: &str,
        cols: &[(&str, &str)],
        right: &[&str],
        query: &HashMap<String, String>,
    ) -> Result<Self> {
        let spec = ListSpec {
            sortable: cols
                .iter()
                .map(|(id, _)| ((*id).into(), SortDir::Asc))
                .collect(),
            default_sort: Some(cols[0].0.into()),
            param_names: ParamNames {
                sort_by: format!("{prefix}_sort"),
                sort_dir: format!("{prefix}_dir"),
                ..ParamNames::default()
            },
            omit_defaults: true,
            ..ListSpec::default()
        }
        .shared()
        .map_err(|_| Error::Internal)?;
        Ok(Self {
            columns: ListColumns::mount(id, columns(cols, right)).map_err(|_| Error::Internal)?,
            list: ListState::parse(query, spec).map_err(|_| Error::Internal)?,
            id,
            event,
        })
    }
    pub(super) fn event(&mut self, payload: &Value) {
        if let ColumnsOutcome::Sort(c) = self.columns.handle(&params::string_values(payload)) {
            self.list.next_sort(&c);
        }
    }
    pub(super) fn params(&mut self, query: &HashMap<String, String>) -> Result<()> {
        self.list = ListState::parse(query, std::sync::Arc::new(self.list.spec().clone()))
            .map_err(|_| Error::Internal)?;
        Ok(())
    }
    pub(super) fn table(
        &self,
        caption: &str,
        rows: Vec<FlexRow>,
        empty: EmptyState,
    ) -> Result<Html> {
        Table {
            id: self.id,
            caption,
            event: self.event,
            columns: &self.columns,
            list: &self.list,
        }
        .render(rows, empty)
    }
}
