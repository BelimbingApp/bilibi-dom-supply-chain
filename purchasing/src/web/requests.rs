//! `/supply-chain/purchase-requests` (Bilimbi
//! `Purchasing.Web.RequestsLive`): the company's purchase requests by status
//! tab in a FlexTable, the draft form with five line rows, and per row the
//! actions its status allows: submit, approve or reject, cancel, and create
//! a draft order from an approved request. Viewing needs
//! `supply-chain.purchasing.view`; each write needs its own capability,
//! read again on every write event (and again on confirmation), and the
//! facade checks it once more. Submitting, deciding and cancelling wait for
//! the shared confirmation dialog because nobody can undo them here; the
//! dialog acts on the action it holds, never on a client-supplied one.
use super::lists::{columns, sort, table, TwoLineCell};
use super::{capabilities, company_id, id, or_empty, outcome, render, text, Core, Flash};
use crate::contributions::REQUESTS_PATH;
use crate::{
    NewRequest, NewRequestLine, OrderFromRequest, Request, RequestQuery, RequestStatus, Supplier,
    APPROVE_REQUESTS, MANAGE_ORDERS, MANAGE_REQUESTS,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::ParamNames,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Badge, BadgeKind, Button, Choice, ColumnsOutcome, ConfirmDialog, EmptyState, Field,
    Filter, FlashMap, FlexRow, Html, InputKind, ListColumns, ListSpec, ListState, RowMode, SortDir,
    Tab, Tabs,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const DENIED: &str = "You do not have permission to work on purchase requests.";
/// How many line rows the draft form offers (Bilimbi `Forms.line_rows/0`).
const LINE_ROWS: usize = 5;
const LINE_FIELDS: [&str; 4] = ["description", "quantity", "unit", "external_item_ref"];
const TABS: [&str; 6] = [
    "all",
    "draft",
    "submitted",
    "approved",
    "rejected",
    "cancelled",
];

/// The tab is Bilimbi's `status` query value, `all` by default; sorting is
/// on the columns Bilimbi shows, in memory over the facade's bounded list,
/// newest number first as the facade orders it.
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("number".into(), SortDir::Desc),
            ("title".into(), SortDir::Asc),
            ("lines".into(), SortDir::Desc),
            ("needed_by".into(), SortDir::Asc),
            ("status".into(), SortDir::Asc),
        ]),
        default_sort: Some("number".into()),
        filters: BTreeMap::from([("status".into(), Filter::one_of(&TABS, "all"))]),
        param_names: ParamNames {
            sort_by: "sort".into(),
            sort_dir: "dir".into(),
            ..ParamNames::default()
        },
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("purchase request list spec")
});

/// A write that waits for confirmation, with the reason typed beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Submit,
    Approve,
    Reject,
    Cancel,
}
struct Pending {
    action: Action,
    request_id: i64,
    reason: String,
}

#[derive(Template)]
#[template(path = "requests.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    can_manage: bool,
    header_fields: Vec<Field>,
    justification: Field,
    lines: Vec<[Field; 4]>,
    tabs: Html,
    requests: Html,
    dialog: Html,
}

/// The controls of one row: what its status allows the reader to do. Each
/// form is its fields in order, then its submit button. Approving and
/// rejecting are two forms, each with its reason, because the live
/// transport posts a form's values without the button that submitted it.
#[derive(Template)]
#[template(path = "request_actions.html")]
struct Actions {
    id: i64,
    submit: Html,
    approve: Vec<Field>,
    reject: Vec<Field>,
    order: Vec<Field>,
    cancel: Vec<Field>,
}

pub(super) struct Requests {
    core: Arc<Core>,
    list: ListState,
    columns: ListColumns,
    rows_changed: bool,
    can_manage: bool,
    can_approve: bool,
    can_order: bool,
    /// The draft form's header values and its line rows, as last posted.
    form: HashMap<String, String>,
    requests: Vec<Request>,
    suppliers: Vec<Supplier>,
    pending: Option<Pending>,
    flash: Flash,
}

fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)
}

impl Requests {
    pub(super) fn new(core: Arc<Core>, query: &HashMap<String, String>) -> Result<Self> {
        let columns = ListColumns::mount(
            "requests-table",
            columns(
                &[
                    ("number", "Number"),
                    ("title", "Title"),
                    ("lines", "Lines"),
                    ("needed_by", "Needed by"),
                    ("status", "Status"),
                ],
                &[],
            ),
        )
        .map_err(|_| Error::Internal)?;
        Ok(Self {
            core,
            list: parse(query)?,
            columns,
            rows_changed: true,
            can_manage: false,
            can_approve: false,
            can_order: false,
            form: HashMap::new(),
            requests: Vec::new(),
            suppliers: Vec::new(),
            pending: None,
            flash: Flash::default(),
        })
    }

    fn tab(&self) -> &str {
        self.list
            .filters
            .get("status")
            .map_or("all", String::as_str)
    }

    pub(super) async fn load(&mut self, scope: &Scope) -> Result<()> {
        let granted = capabilities(&self.core, scope).await?;
        self.can_manage = granted.contains(MANAGE_REQUESTS);
        self.can_approve = granted.contains(APPROVE_REQUESTS);
        self.can_order = granted.contains(MANAGE_ORDERS);
        if !self.rows_changed {
            return Ok(());
        }
        let company = company_id(scope);
        let purchasing = &self.core.purchasing;
        let query = RequestQuery {
            status: RequestStatus::parse(self.tab()),
            ..RequestQuery::default()
        };
        self.requests = or_empty(purchasing.list_requests(scope, company, query).await)?;
        self.suppliers = if self.can_order {
            or_empty(purchasing.list_suppliers(scope, company, true).await)?
        } else {
            Vec::new()
        };
        self.columns
            .load(&self.requests, |request| request.id.to_string());
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, payload: &Value) -> Result<()> {
        let kind = payload
            .get("type")
            .and_then(Value::as_str)
            .ok_or(Error::Invalid)?;
        match kind {
            "requests_grid" => {
                self.rows_changed = false;
                if let ColumnsOutcome::Sort(column) =
                    self.columns.handle(&params::string_values(payload))
                {
                    if !self.list.next_sort(&column) {
                        return Err(Error::Invalid);
                    }
                }
            }
            "clear-flash" => {
                self.rows_changed = false;
                self.flash.clear(payload.get("key").and_then(Value::as_str));
            }
            "dismiss" => {
                self.rows_changed = false;
                self.pending = None;
            }
            "save_request" | "submit" | "decide" | "cancel" | "create_order" | "confirm" => {
                // The capability is read again for every write event; the
                // facade checks it a third time when the write runs.
                let granted = capabilities(&self.core, scope).await?;
                self.can_manage = granted.contains(MANAGE_REQUESTS);
                self.can_approve = granted.contains(APPROVE_REQUESTS);
                self.can_order = granted.contains(MANAGE_ORDERS);
                if self.permitted(kind) {
                    self.write(scope, kind, payload).await?;
                } else {
                    self.flash.error(DENIED);
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    fn permitted(&self, kind: &str) -> bool {
        match kind {
            "save_request" | "submit" | "cancel" => self.can_manage,
            "decide" => self.can_approve,
            "create_order" => self.can_order,
            _ => match self.pending.as_ref().map(|pending| pending.action) {
                None => true,
                Some(Action::Submit | Action::Cancel) => self.can_manage,
                Some(Action::Approve | Action::Reject) => self.can_approve,
            },
        }
    }

    /// Holds a confirmed action until the dialog answers; a request the
    /// form no longer names is refused at once.
    fn hold(&mut self, action: Action, payload: &Value) {
        match id(payload, "request_id") {
            Some(request_id) => {
                self.pending = Some(Pending {
                    action,
                    request_id,
                    reason: text(payload, "reason").to_owned(),
                })
            }
            None => self
                .flash
                .error("That purchase request is no longer available."),
        }
    }

    async fn write(&mut self, scope: &Scope, kind: &str, payload: &Value) -> Result<()> {
        match kind {
            "save_request" => self.save(scope, payload).await?,
            "submit" => self.hold(Action::Submit, payload),
            "decide" => match text(payload, "decision") {
                "approve" => self.hold(Action::Approve, payload),
                "reject" => self.hold(Action::Reject, payload),
                _ => return Err(Error::Invalid),
            },
            "cancel" => self.hold(Action::Cancel, payload),
            "create_order" => self.create_order(scope, payload).await?,
            "confirm" => {
                if let Some(pending) = self.pending.take() {
                    self.run(scope, pending).await?;
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    async fn save(&mut self, scope: &Scope, payload: &Value) -> Result<()> {
        let form: HashMap<String, String> = payload
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, _)| key.starts_with("request["))
            .filter_map(|(key, value)| {
                let value = value.as_str().filter(|value| value.len() <= 1024)?;
                Some((key.clone(), value.to_owned()))
            })
            .take(64)
            .collect();
        let value = |key: &str| form.get(key).cloned().unwrap_or_default();
        // Rows without a description are ignored, as the form says.
        let lines = (0..LINE_ROWS)
            .map(|row| NewRequestLine {
                description: value(&line_name(row, "description")),
                external_item_ref: value(&line_name(row, "external_item_ref")),
                quantity: value(&line_name(row, "quantity")),
                unit: value(&line_name(row, "unit")),
            })
            .filter(|line| !line.description.trim().is_empty())
            .collect();
        let request = NewRequest {
            title: value("request[title]"),
            justification: value("request[justification]"),
            needed_on: value("request[needed_on]"),
            lines,
        };
        let result = self
            .core
            .purchasing
            .create_request(scope, company_id(scope), &request)
            .await;
        match outcome(result)? {
            Ok(request) => {
                self.form.clear();
                self.flash.success(format!(
                    "Purchase request {} saved as a draft.",
                    request.request_number
                ));
            }
            Err(message) => {
                self.form = form;
                self.flash.error(message);
            }
        }
        Ok(())
    }

    async fn create_order(&mut self, scope: &Scope, payload: &Value) -> Result<()> {
        let Some(request_id) = id(payload, "request_id") else {
            self.flash
                .error("That purchase request is no longer available.");
            return Ok(());
        };
        let order = OrderFromRequest {
            supplier_id: id(payload, "supplier_id"),
            currency_code: text(payload, "currency_code").to_owned(),
            ..OrderFromRequest::default()
        };
        let result = self
            .core
            .purchasing
            .create_order_from_request(scope, company_id(scope), request_id, &order)
            .await;
        match outcome(result)? {
            // TODO(S4): Bilimbi navigates to the order's page with this
            // flash (`push_navigate` to `/supply-chain/purchase-orders/:id`);
            // switch to `LiveBody.redirect` once that page is contributed.
            Ok(order) => self.flash.success(format!(
                "Purchase order {} drafted from the request.",
                order.order_number
            )),
            Err(message) => self.flash.error(message),
        }
        Ok(())
    }

    /// Runs a confirmed action; the facade authorizes it for the scope's
    /// actor and records who performed it.
    async fn run(&mut self, scope: &Scope, pending: Pending) -> Result<()> {
        let purchasing = &self.core.purchasing;
        let company = company_id(scope);
        let id = pending.request_id;
        let (result, message) = match pending.action {
            Action::Submit => (
                purchasing.submit_request(scope, company, id).await,
                "Request submitted for approval.",
            ),
            Action::Approve => (
                purchasing
                    .approve_request(scope, company, id, &pending.reason)
                    .await,
                "Request approved.",
            ),
            Action::Reject => (
                purchasing
                    .reject_request(scope, company, id, &pending.reason)
                    .await,
                "Request rejected.",
            ),
            Action::Cancel => (
                purchasing
                    .cancel_request(scope, company, id, &pending.reason)
                    .await,
                "Request cancelled.",
            ),
        };
        match outcome(result)? {
            Ok(_) => self.flash.success(message),
            Err(refusal) => self.flash.error(refusal),
        }
        Ok(())
    }

    pub(super) fn render(&self) -> Result<String> {
        let value = |key: &str| self.form.get(key).cloned().unwrap_or_default();
        let header_fields = vec![
            Field::in_form("request", "title")
                .label("Title")
                .value(value("request[title]")),
            Field::in_form("request", "needed_on")
                .label("Needed by")
                .kind(InputKind::Date)
                .value(value("request[needed_on]")),
        ];
        let justification = Field::in_form("request", "justification")
            .label("Justification")
            .kind(InputKind::Textarea)
            .value(value("request[justification]"));
        let lines = (0..LINE_ROWS)
            .map(|row| {
                LINE_FIELDS.map(|field| {
                    let label = match field {
                        "external_item_ref" => "item reference",
                        other => other,
                    };
                    let control = Field::named(line_name(row, field))
                        .id(format!("request_lines_{row}_{field}"))
                        .aria_label(format!("Line {} {label}", row + 1))
                        .value(value(&line_name(row, field)));
                    match field {
                        "quantity" => control.attr("inputmode", "decimal").wrapper_class("w-24"),
                        "unit" => control.wrapper_class("w-24"),
                        _ => control.wrapper_class("w-full"),
                    }
                })
            })
            .collect();
        View {
            flash: self.flash.map(),
            can_manage: self.can_manage,
            header_fields,
            justification,
            lines,
            tabs: self.tabs()?,
            requests: self.table()?,
            dialog: self.dialog()?,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn tabs(&self) -> Result<Html> {
        let hrefs: Vec<String> = TABS
            .iter()
            .map(|tab| {
                params::address(
                    REQUESTS_PATH,
                    &HashMap::from([("status".to_owned(), (*tab).to_owned())]),
                )
            })
            .collect();
        let ids: Vec<String> = TABS
            .iter()
            .map(|tab| format!("request-tab-{tab}"))
            .collect();
        let tabs = TABS
            .iter()
            .zip(&hrefs)
            .zip(&ids)
            .map(|((tab, href), id)| {
                let label = if *tab == "all" {
                    "All".to_owned()
                } else {
                    status_label(tab)
                };
                Tab {
                    id,
                    ..Tab::patch(href, &label, self.tab() == *tab)
                }
            })
            .collect();
        render(&Tabs {
            id: "request-tabs",
            label: "Request statuses",
            class: "mt-5",
            tabs,
        })
    }

    fn table(&self) -> Result<Html> {
        let compact = self.columns.mode() == RowMode::Compact;
        let mut requests: Vec<&Request> = self.requests.iter().collect();
        sort(
            &mut requests,
            &self.list,
            |a: &&Request, b: &&Request, column| match column {
                "title" => a.title.cmp(&b.title),
                "lines" => a.lines.len().cmp(&b.lines.len()),
                "needed_by" => a.needed_on.cmp(&b.needed_on),
                "status" => a.status.as_str().cmp(b.status.as_str()),
                _ => a.request_number.cmp(&b.request_number),
            },
        );
        let mut rows = Vec::with_capacity(requests.len());
        let mut any_actions = false;
        for request in requests {
            let title = render(&TwoLineCell {
                first: &request.title,
                second: request.decision_reason.as_deref().filter(|_| !compact),
            })?;
            let mut row = FlexRow::new(request.id.to_string())
                .slot("number", Html::text(&request.request_number.to_string()))
                .slot("title", title)
                .slot("lines", Html::text(&request.lines.len().to_string()))
                .slot(
                    "needed_by",
                    Html::text(request.needed_on.as_deref().unwrap_or("—")),
                )
                .slot(
                    "status",
                    render(&Badge::status(
                        status_kind(request.status),
                        &status_label(request.status.as_str()),
                    ))?,
                );
            row.actions = self.actions(request)?;
            any_actions |= !row.actions.is_empty();
            rows.push(row);
        }
        let reason = if self.tab() == "all" {
            "Requests you create appear here.".to_owned()
        } else {
            format!("No request is {}.", status_label(self.tab()).to_lowercase())
        };
        table(
            "requests-table",
            "Purchase requests",
            "requests_grid",
            &self.columns,
            &self.list,
            rows,
            any_actions || self.can_manage || self.can_approve || self.can_order,
            EmptyState::missing("No purchase requests here", reason),
        )
    }

    fn actions(&self, request: &Request) -> Result<Html> {
        use RequestStatus::{Approved, Draft, Submitted};
        let id = request.id;
        let status = request.status;
        let hidden = |form: &str, name: &str, value: String| {
            Field::named(name)
                .id(format!("{form}-{id}-{}", name.replace('_', "-")))
                .kind(InputKind::Hidden)
                .value(value)
        };
        let reason = |form: &str, label: &str| {
            Field::named("reason")
                .id(format!("{form}-{id}-reason"))
                .aria_label(label)
                .placeholder("Reason")
                .attr("maxlength", "500")
                .wrapper_class("w-40")
        };
        let submit = if self.can_manage && status == Draft {
            let value = id.to_string();
            render(&Button {
                id: &format!("request-submit-{id}"),
                button_type: "button",
                event: "submit",
                name: "request_id",
                value: &value,
                body: Html::text("Submit"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        let decision = |form: &str, decision: &str| {
            if self.can_approve && status == Submitted {
                vec![
                    hidden(form, "request_id", id.to_string()),
                    hidden(form, "decision", decision.to_owned()),
                    reason(form, "Reason"),
                ]
            } else {
                Vec::new()
            }
        };
        let approve = decision("decide", "approve");
        let reject = decision("reject", "reject");
        let order = if self.can_order && status == Approved {
            vec![
                hidden("order-from", "request_id", id.to_string()),
                Field::named("supplier_id")
                    .id(format!("order-from-{id}-supplier"))
                    .kind(InputKind::Select)
                    .aria_label("Supplier")
                    .prompt("Supplier…")
                    .options(
                        self.suppliers
                            .iter()
                            .map(|supplier| Choice::new(&supplier.name, supplier.id.to_string()))
                            .collect(),
                    )
                    .wrapper_class("w-40"),
                Field::named("currency_code")
                    .id(format!("order-from-{id}-currency"))
                    .aria_label("Currency")
                    .placeholder("Currency")
                    .attr("maxlength", "3")
                    .wrapper_class("w-24"),
            ]
        } else {
            Vec::new()
        };
        let cancel = if self.can_manage && matches!(status, Draft | Submitted | Approved) {
            vec![
                hidden("cancel", "request_id", id.to_string()),
                reason("cancel", "Cancellation reason"),
            ]
        } else {
            Vec::new()
        };
        if submit.is_empty() && approve.is_empty() && order.is_empty() && cancel.is_empty() {
            return Ok(Html::EMPTY);
        }
        render(&Actions {
            id,
            submit,
            approve,
            reject,
            order,
            cancel,
        })
    }

    fn dialog(&self) -> Result<Html> {
        let Some(pending) = &self.pending else {
            return Ok(Html::EMPTY);
        };
        let subject = self
            .requests
            .iter()
            .find(|request| request.id == pending.request_id)
            .map_or("This purchase request".to_owned(), |request| {
                format!("Purchase request {}", request.request_number)
            });
        let (consequence, detail, confirm, working) = match pending.action {
            Action::Submit => (
                format!("{subject} will be submitted for approval."),
                "Its title and lines are fixed from then on; an approver decides it.",
                "Submit",
                "Submitting…",
            ),
            Action::Approve => (
                format!("{subject} will be approved."),
                "The decision is recorded with your name and cannot be undone.",
                "Approve",
                "Approving…",
            ),
            Action::Reject => (
                format!("{subject} will be rejected."),
                "The requester sees your reason. This cannot be undone.",
                "Reject",
                "Rejecting…",
            ),
            Action::Cancel => (
                format!("{subject} will be cancelled."),
                "It keeps its number and history. This cannot be undone.",
                "Cancel request",
                "Cancelling…",
            ),
        };
        render(&ConfirmDialog {
            id: "request-confirm",
            consequence: &consequence,
            detail,
            confirm,
            working,
            confirm_event: "confirm",
            cancel_event: "dismiss",
        })
    }
}

impl LiveState<Scope, Error> for Requests {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    // Bilimbi's `handle_params`: another tab drops a held action.
                    self.list = parse(values)?;
                    self.pending = None;
                }
                LiveInput::Event(payload) => self.event(request.scope, payload).await?,
            }
            self.load(request.scope).await?;
            Ok(LiveBody {
                html: self.render()?,
                query: self.list.to_params(),
                rows: self.requests.len(),
                ..LiveBody::default()
            })
        })
    }
}

fn line_name(row: usize, field: &str) -> String {
    format!("request[lines][{row}][{field}]")
}

/// A status as a person reads it (Bilimbi `Forms.status_label/1`).
fn status_label(status: &str) -> String {
    let words = status.replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The badge a status renders with (Bilimbi `Forms.status_kind/1`).
fn status_kind(status: RequestStatus) -> BadgeKind {
    match status {
        RequestStatus::Draft => BadgeKind::Neutral,
        RequestStatus::Submitted => BadgeKind::Warning,
        RequestStatus::Approved => BadgeKind::Success,
        RequestStatus::Rejected | RequestStatus::Cancelled => BadgeKind::Danger,
    }
}
