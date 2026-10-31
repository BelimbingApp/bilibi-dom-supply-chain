//! Purchase order detail (Bilimbi `Purchasing.Web.OrderLive`). Transitions
//! retain their action and reason on the server until the dialog answers.
use super::lists::{columns, sort, table, TwoLineCell};
use super::orders::{badge, label, supplier};
use super::{capabilities, outcome, render, text, Core, Flash};
use crate::{Order, OrderStatus, APPROVE_ORDERS, MANAGE_ORDERS};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, Format, Precision, Rendered};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::ParamNames,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BackLink, Button, ButtonVariant, ColumnsOutcome, ConfirmDialog, EmptyState, Fact,
    Facts, Field, FlashMap, FlexRow, Html, InputKind, ListColumns, ListSpec, ListState, RowMode,
    SortDir,
};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc, time::SystemTime};

fn company_id(scope: &Scope) -> i64 {
    scope.current_company().map_or(0, |id| id.get())
}

const DENIED: &str = "You do not have permission to change purchase orders.";
const ORDERS_PATH: &str = "/supply-chain/purchase-orders";
#[derive(Clone, Copy)]
enum Action {
    Submit,
    Approve,
    Return,
    Send,
    Close,
    Cancel,
}
impl Action {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "submit" => Self::Submit,
            "approve" => Self::Approve,
            "return" => Self::Return,
            "send" => Self::Send,
            "close" => Self::Close,
            "cancel" => Self::Cancel,
            _ => return None,
        })
    }
    fn key(self) -> &'static str {
        match self {
            Self::Submit => "submit",
            Self::Approve => "approve",
            Self::Return => "return",
            Self::Send => "send",
            Self::Close => "close",
            Self::Cancel => "cancel",
        }
    }
    fn permitted(self, manage: bool, approve: bool) -> bool {
        match self {
            Self::Approve | Self::Return => approve,
            _ => manage,
        }
    }
}
struct Pending {
    action: Action,
    reason: String,
}
struct Grid {
    columns: ListColumns,
    list: ListState,
    id: &'static str,
    event: &'static str,
}
impl Grid {
    fn new(
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
    fn event(&mut self, payload: &Value) {
        if let ColumnsOutcome::Sort(c) = self.columns.handle(&params::string_values(payload)) {
            self.list.next_sort(&c);
        }
    }
    fn table(&self, caption: &str, rows: Vec<FlexRow>, empty: EmptyState) -> Result<Html> {
        table(
            self.id,
            caption,
            self.event,
            &self.columns,
            &self.list,
            rows,
            false,
            empty,
        )
    }
}
#[derive(Template)]
#[template(path = "order.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    order: Option<&'a Order>,
    back: Html,
    status_line: Html,
    facts: Html,
    lines: Html,
    actions: Html,
    history: Html,
    amendments: Html,
    dialog: Html,
    missing: Html,
    amendment_editor: Html,
}
#[derive(Template)]
#[template(path = "order_actions.html")]
struct Actions {
    buttons: Vec<Html>,
    forms: Vec<ActionForm>,
    final_status: Option<String>,
    forbidden: bool,
}
struct ActionForm {
    id: &'static str,
    action: Field,
    reason: Field,
    button: Html,
}
#[derive(Template)]
#[template(path = "order_instant.html")]
struct Instant {
    id: String,
    value: Rendered,
}
#[derive(Template)]
#[template(path = "order_subtitle.html")]
struct Subtitle<'a> {
    badge: Html,
    supplier: &'a str,
    currency: &'a str,
}
#[derive(Template)]
#[template(path = "order_amendment_cell.html")]
struct AmendmentCell<'a> {
    reason: &'a str,
    added: usize,
    replaced: usize,
    compact: bool,
}

pub(super) struct OrderPage {
    core: Arc<Core>,
    id: i64,
    order: Option<Order>,
    can_manage: bool,
    can_approve: bool,
    pending: Option<Pending>,
    amendment: Option<super::order_amend::Editor>,
    flash: Flash,
    lines: Grid,
    history: Grid,
    amendments: Grid,
    display: DisplayContext,
}
impl OrderPage {
    pub(super) fn new(
        core: Arc<Core>,
        path: &HashMap<String, String>,
        query: &HashMap<String, String>,
    ) -> Result<Self> {
        Ok(Self {
            core,
            id: params::positive_id(path.get("id").map(String::as_str)).unwrap_or(0),
            order: None,
            can_manage: false,
            can_approve: false,
            pending: None,
            amendment: None,
            flash: Flash::default(),
            lines: Grid::new(
                "order-lines-table",
                "lines_grid",
                "lines",
                &[
                    ("number", "#"),
                    ("description", "Description"),
                    ("quantity", "Quantity"),
                    ("price", "Unit price"),
                    ("tax", "Tax %"),
                    ("total", "Line total"),
                    ("received", "Received"),
                ],
                &["quantity", "price", "tax", "total", "received"],
                query,
            )?,
            history: Grid::new(
                "order-events",
                "history_grid",
                "history",
                &[
                    ("at", "When"),
                    ("status", "Status"),
                    ("from", "From"),
                    ("actor", "By"),
                    ("reason", "Reason"),
                ],
                &[],
                query,
            )?,
            amendments: Grid::new(
                "order-amendment-list",
                "amendments_grid",
                "amendments",
                &[
                    ("number", "Amendment"),
                    ("actor", "By"),
                    ("at", "When"),
                    ("reason", "Reason"),
                ],
                &[],
                query,
            )?,
            display: DisplayContext::anonymous(),
        })
    }
    pub(super) fn title(&self) -> String {
        self.order.as_ref().map_or("Purchase order".into(), |o| {
            format!("Purchase order {}", o.order_number)
        })
    }
    pub(super) async fn load(&mut self, scope: &Scope, rows_changed: bool) -> Result<()> {
        let grants = capabilities(&self.core, scope).await?;
        self.can_manage = grants.contains(MANAGE_ORDERS);
        self.can_approve = grants.contains(APPROVE_ORDERS);
        let client = self.core.db.get().await?;
        let locale = self
            .core
            .locales
            .resolve(&**client, scope, None)
            .await?
            .locale;
        self.display = self
            .core
            .display
            .display(&**client, scope, None, locale)
            .await?;
        if !rows_changed {
            return Ok(());
        }
        self.order = match self
            .core
            .purchasing
            .get_order(scope, company_id(scope), self.id)
            .await
        {
            Ok(o) => o,
            Err(Error::NotFound | Error::Forbidden) => None,
            Err(e) => return Err(e),
        };
        if let Some(o) = &self.order {
            self.lines.columns.load(&o.lines, |l| l.id.to_string());
            self.history.columns.load(&o.events, |e| e.id.to_string());
            self.amendments
                .columns
                .load(&o.amendments, |a| a.id.to_string());
        } else {
            self.pending = None;
            self.amendment = None;
        }
        Ok(())
    }
    async fn event(&mut self, scope: &Scope, payload: &Value) -> Result<bool> {
        match text(payload, "type") {
            "lines_grid" => self.lines.event(payload),
            "history_grid" => self.history.event(payload),
            "amendments_grid" => self.amendments.event(payload),
            "clear-flash" => self.flash.clear(payload.get("key").and_then(Value::as_str)),
            "dismiss" => self.pending = None,
            "transition" | "confirm" => {
                let grants = capabilities(&self.core, scope).await?;
                self.can_manage = grants.contains(MANAGE_ORDERS);
                self.can_approve = grants.contains(APPROVE_ORDERS);
                let action = if text(payload, "type") == "transition" {
                    Action::parse(text(payload, "action"))
                } else {
                    self.pending.as_ref().map(|p| p.action)
                };
                // An unknown action is not permitted, as Bilimbi's `permitted?/3`.
                let Some(action) = action else {
                    if text(payload, "type") == "transition" {
                        self.flash.error(DENIED);
                    }
                    return Ok(true);
                };
                if !action.permitted(self.can_manage, self.can_approve) {
                    self.flash.error(DENIED);
                } else if text(payload, "type") == "transition" {
                    self.pending = Some(Pending {
                        action,
                        reason: text(payload, "reason").chars().take(4096).collect(),
                    });
                } else if let Some(pending) = self.pending.take() {
                    self.run(scope, pending).await?;
                }
                return Ok(true);
            }
            _ => return Err(Error::Invalid),
        }
        Ok(false)
    }
    async fn run(&mut self, scope: &Scope, pending: Pending) -> Result<()> {
        let p = &self.core.purchasing;
        let company = company_id(scope);
        let id = self.id;
        let reason = &pending.reason;
        let (result, message) = match pending.action {
            Action::Submit => (
                p.submit_order(scope, company, id).await,
                "Order submitted for approval.",
            ),
            Action::Approve => (
                p.approve_order(scope, company, id, reason).await,
                "Order approved.",
            ),
            Action::Return => (
                p.return_order(scope, company, id, reason).await,
                "Order returned to draft.",
            ),
            Action::Send => (
                p.send_order(scope, company, id).await,
                "Order marked as sent.",
            ),
            Action::Close => (
                p.close_order(scope, company, id, reason).await,
                "Order closed.",
            ),
            Action::Cancel => (
                p.cancel_order(scope, company, id, reason).await,
                "Order cancelled.",
            ),
        };
        match outcome(result)? {
            Ok(_) => self.flash.success(message),
            Err(message) => self.flash.error(message),
        };
        Ok(())
    }
    fn actions(&self, o: &Order) -> Result<Html> {
        let button = |id: &str, label: &str, action: Action| {
            render(&Button {
                id,
                button_type: "button",
                event: "transition",
                name: "action",
                value: action.key(),
                body: Html::text(label),
                ..Button::default()
            })
        };
        let mut buttons = vec![];
        let mut forms = vec![];
        if self.can_manage && matches!(o.status, OrderStatus::Approved | OrderStatus::Sent) {
            buttons.push(render(&Button {
                id: "order-amend",
                event: "amend_open",
                body: Html::text("Amend"),
                ..Button::default()
            })?);
        }
        let mut add =
            |id: &'static str, label: &str, action: Action, reason_label: &str| -> Result<()> {
                forms.push(ActionForm {
                    id,
                    action: Field::named("action")
                        .id(format!("{id}-action"))
                        .kind(InputKind::Hidden)
                        .value(action.key()),
                    reason: Field::named("reason")
                        .id(format!("{id}-reason"))
                        .aria_label(reason_label)
                        .placeholder("Reason")
                        .attr("maxlength", "500")
                        .wrapper_class("w-48"),
                    button: render(&Button {
                        button_type: "submit",
                        variant: match action {
                            Action::Approve => ButtonVariant::Primary,
                            Action::Cancel => ButtonVariant::Danger,
                            _ => ButtonVariant::Secondary,
                        },
                        body: Html::text(label),
                        ..Button::default()
                    })?,
                });
                Ok(())
            };
        if self.can_manage && o.status == OrderStatus::Draft {
            buttons.push(button(
                "order-submit",
                "Submit for approval",
                Action::Submit,
            )?);
        }
        if self.can_approve && o.status == OrderStatus::Submitted {
            add("order-decide", "Approve", Action::Approve, "Reason")?;
            if o.amendment_count == 0 {
                add("order-return", "Return to draft", Action::Return, "Reason")?;
            }
        }
        if self.can_manage && o.status == OrderStatus::Approved {
            buttons.push(button("order-send", "Mark as sent", Action::Send)?);
        }
        if self.can_manage && matches!(o.status, OrderStatus::Sent | OrderStatus::PartiallyReceived)
        {
            add("order-close", "Close", Action::Close, "Closing reason")?;
        }
        let final_status = matches!(o.status, OrderStatus::Closed | OrderStatus::Cancelled);
        if self.can_manage && !final_status {
            add(
                "order-cancel",
                "Cancel order",
                Action::Cancel,
                "Cancellation reason",
            )?;
        }
        render(&Actions {
            buttons,
            forms,
            final_status: final_status.then(|| label(o.status.as_str()).to_lowercase()),
            forbidden: !self.can_manage && !self.can_approve && !final_status,
        })
    }
    fn instant(&self, id: String, at: SystemTime) -> Result<Html> {
        render(&Instant {
            id,
            value: self.display.render(
                jiff::Timestamp::try_from(at).map_err(|_| Error::Internal)?,
                Format::DateTime,
                Precision::Second,
            ),
        })
    }
    fn dialog(&self, o: &Order) -> Result<Html> {
        let Some(pending) = &self.pending else {
            return Ok(Html::EMPTY);
        };
        let subject = format!("Purchase order {}", o.order_number);
        let (consequence,detail,confirm,working)=match pending.action {
            Action::Submit=>(format!("{subject} will be submitted for approval."),"Its supplier, currency and lines are fixed from then on unless an approver returns it.","Submit","Submitting…"),
            Action::Approve=>(format!("{subject} will be approved."),"From then on it changes only through a recorded amendment. This cannot be undone.","Approve","Approving…"),
            Action::Return=>(format!("{subject} will go back to draft."),"The submitter sees your reason and may resubmit it.","Return","Returning…"),
            Action::Send=>(format!("{subject} will be marked as sent to {}.",supplier(o)),"Receipts can be recorded against it from then on. This cannot be undone.","Mark sent","Marking…"),
            Action::Close=>(format!("{subject} will be closed."),"No further receipt can be recorded against it. This cannot be undone.","Close","Closing…"),
            Action::Cancel=>(format!("{subject} will be cancelled."),"It keeps its number, lines and history. This cannot be undone.","Cancel order","Cancelling…"),
        };
        render(&ConfirmDialog {
            id: "order-confirm",
            consequence: &consequence,
            detail,
            confirm,
            working,
            confirm_event: "confirm",
            cancel_event: "dismiss",
        })
    }
    pub(super) fn render(&self) -> Result<String> {
        let mut view = View {
            flash: self.flash.map(),
            order: self.order.as_ref(),
            back: render(&BackLink {
                href: ORDERS_PATH,
                title: "Back to purchase orders",
                ..BackLink::default()
            })?,
            status_line: Html::EMPTY,
            facts: Html::EMPTY,
            lines: Html::EMPTY,
            actions: Html::EMPTY,
            history: Html::EMPTY,
            amendments: Html::EMPTY,
            dialog: Html::EMPTY,
            amendment_editor: match &self.amendment {
                Some(editor) => editor.render()?,
                None => Html::EMPTY,
            },
            missing: render(
                &EmptyState::missing(
                    "This purchase order is not available.",
                    "It may belong to another company, or the link is out of date.",
                )
                .id("order-missing")
                .action(render(&Button {
                    href: ORDERS_PATH,
                    body: Html::text("Back to purchase orders"),
                    ..Button::default()
                })?),
            )?,
        };
        if let Some(o) = &self.order {
            view.status_line = render(&Subtitle {
                badge: badge(o.status)?,
                supplier: supplier(o),
                currency: &o.currency_code,
            })?;
            let money = |v| format!("{v} {}", o.currency_code);
            view.facts = render(&Facts {
                id: "order-facts",
                items: vec![
                    ("Supplier", supplier(o).to_owned()),
                    (
                        "Supplier reference",
                        o.reference.clone().unwrap_or("—".into()),
                    ),
                    (
                        "Expected delivery",
                        o.expected_on.clone().unwrap_or("—".into()),
                    ),
                    (
                        "From request",
                        o.purchase_request_number
                            .map_or("—".into(), |v| v.to_string()),
                    ),
                    ("Subtotal", money(o.subtotal)),
                    ("Tax", money(o.tax_total)),
                    ("Total", money(o.total)),
                    ("Amendments", o.amendment_count.to_string()),
                    ("Notes", o.notes.clone().unwrap_or("—".into())),
                ]
                .into_iter()
                .map(|(title, value)| Fact {
                    title,
                    value: Html::text(&value),
                    ..Fact::default()
                })
                .collect(),
            })?;
            let compact = self.lines.columns.mode() == RowMode::Compact;
            let mut lines: Vec<_> = o.lines.iter().collect();
            sort(&mut lines, &self.lines.list, |a, b, c| match c {
                "description" => a.description.cmp(&b.description),
                "quantity" => a.quantity.cmp(&b.quantity),
                "price" => a.unit_price.cmp(&b.unit_price),
                "tax" => a.tax_rate.cmp(&b.tax_rate),
                "total" => a.line_total.cmp(&b.line_total),
                "received" => a.received_quantity.cmp(&b.received_quantity),
                _ => a.line_number.cmp(&b.line_number),
            });
            let rows = lines
                .into_iter()
                .map(|l| {
                    Ok(FlexRow::new(l.id.to_string())
                        .slot("number", Html::text(&l.line_number.to_string()))
                        .slot(
                            "description",
                            render(&TwoLineCell {
                                first: &l.description,
                                second: l.external_item_ref.as_deref().filter(|_| !compact),
                            })?,
                        )
                        .slot(
                            "quantity",
                            Html::text(&format!("{} {}", l.quantity, l.unit)),
                        )
                        .slot("price", Html::text(&l.unit_price.to_string()))
                        .slot(
                            "tax",
                            Html::text(&l.tax_rate.map_or("—".into(), |v| v.to_string())),
                        )
                        .slot("total", Html::text(&l.line_total.to_string()))
                        .slot("received", Html::text(&l.received_quantity.to_string())))
                })
                .collect::<Result<Vec<_>>>()?;
            view.lines = self.lines.table(
                "Order lines",
                rows,
                EmptyState::missing(
                    "No lines",
                    "A draft needs at least one line before it is submitted.",
                ),
            )?;
            view.actions = self.actions(o)?;
            let mut events: Vec<_> = o.events.iter().collect();
            sort(&mut events, &self.history.list, |a, b, c| match c {
                "status" => a.to_status.as_str().cmp(b.to_status.as_str()),
                "from" => a
                    .from_status
                    .map(OrderStatus::as_str)
                    .cmp(&b.from_status.map(OrderStatus::as_str)),
                "actor" => a.actor_user_id.cmp(&b.actor_user_id),
                "reason" => a.reason.cmp(&b.reason),
                _ => a.occurred_at.cmp(&b.occurred_at),
            });
            let rows =
                events
                    .into_iter()
                    .map(|e| {
                        Ok(FlexRow::new(format!("order-event-{}", e.id))
                            .slot(
                                "at",
                                self.instant(format!("order-event-at-{}", e.id), e.occurred_at)?,
                            )
                            .slot("status", badge(e.to_status)?)
                            .slot(
                                "from",
                                Html::text(&e.from_status.map_or("—".into(), |s| {
                                    format!("from {}", label(s.as_str()))
                                })),
                            )
                            .slot(
                                "actor",
                                Html::text(&format!(
                                    "by user {}{}",
                                    e.actor_user_id,
                                    e.actor_impersonator_id
                                        .map_or(String::new(), |id| format!(" (as user {id})"))
                                )),
                            )
                            .slot("reason", Html::text(e.reason.as_deref().unwrap_or("—"))))
                    })
                    .collect::<Result<Vec<_>>>()?;
            view.history = self.history.table(
                "History",
                rows,
                EmptyState::missing("No history", "Status changes appear here."),
            )?;
            let mut amendments: Vec<_> = o.amendments.iter().collect();
            sort(&mut amendments, &self.amendments.list, |a, b, c| match c {
                "actor" => a.amended_by_user_id.cmp(&b.amended_by_user_id),
                "at" => a.occurred_at.cmp(&b.occurred_at),
                "reason" => a.reason.cmp(&b.reason),
                _ => a.amendment_number.cmp(&b.amendment_number),
            });
            let rows = amendments
                .into_iter()
                .map(|a| {
                    Ok(FlexRow::new(format!("order-amendment-{}", a.id))
                        .slot(
                            "number",
                            Html::text(&format!("Amendment {}", a.amendment_number)),
                        )
                        .slot(
                            "actor",
                            Html::text(&format!("by user {}", a.amended_by_user_id)),
                        )
                        .slot(
                            "at",
                            self.instant(format!("order-amendment-at-{}", a.id), a.occurred_at)?,
                        )
                        .slot(
                            "reason",
                            render(&AmendmentCell {
                                reason: &a.reason,
                                added: a.lines.len(),
                                replaced: a.superseded_line_ids.len(),
                                compact: self.amendments.columns.mode() == RowMode::Compact,
                            })?,
                        ))
                })
                .collect::<Result<Vec<_>>>()?;
            if !o.amendments.is_empty() {
                view.amendments = self.amendments.table(
                    "Amendments",
                    rows,
                    EmptyState::missing("No amendments", "Recorded amendments appear here."),
                )?;
            }
            view.dialog = self.dialog(o)?;
        }
        view.render().map_err(|_| Error::Internal)
    }
}
impl LiveState<Scope, Error> for OrderPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let changed = match request.input {
                LiveInput::Refresh => true,
                LiveInput::Params(q) => {
                    for grid in [&mut self.lines, &mut self.history, &mut self.amendments] {
                        grid.list = ListState::parse(q, Arc::new(grid.list.spec().clone()))
                            .map_err(|_| Error::Internal)?;
                    }
                    self.pending = None;
                    self.amendment = None;
                    true
                }
                LiveInput::Event(p) if text(p, "type").starts_with("amend_") => {
                    let grants = capabilities(&self.core, request.scope).await?;
                    if !grants.contains(MANAGE_ORDERS) {
                        self.amendment = None;
                        self.flash.error(DENIED);
                    } else if text(p, "type") == "amend_cancel" {
                        self.amendment = None;
                    } else if text(p, "type") == "amend_open" {
                        self.pending = None;
                        if let Some(o) = &self.order {
                            let taxes = self
                                .core
                                .purchasing
                                .list_tax_codes(request.scope, company_id(request.scope), false)
                                .await?;
                            self.amendment = Some(super::order_amend::Editor::new(
                                company_id(request.scope),
                                o,
                                taxes,
                            ));
                        }
                    } else if let Some(editor) = &mut self.amendment {
                        if editor
                            .event(&self.core, request.scope, request.credential, p)
                            .await?
                        {
                            self.amendment = None;
                            self.flash.success("Order amended.");
                        }
                    }
                    true
                }
                LiveInput::Event(p) => self.event(request.scope, p).await?,
            };
            self.load(request.scope, changed).await?;
            let mut query = self.lines.list.to_params();
            query.extend(self.history.list.to_params());
            query.extend(self.amendments.list.to_params());
            Ok(LiveBody {
                html: self.render()?,
                query,
                ..LiveBody::default()
            })
        })
    }
}
