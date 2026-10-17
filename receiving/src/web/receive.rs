//! Receive goods against one sent purchase order and save a draft receipt
//! (Bilimbi `Receiving.Web.ReceiveLive`). Choosing an order is a read: the
//! page keeps it in its address (`?order=<id>`).
use super::{capabilities, company_id, label, outcome, render, text, Core, Flash};
use crate::contributions::RECEIPTS_PATH;
use crate::{NewReceipt, NewReceiptLine, MANAGE_RECEIPTS};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_supply_chain_purchasing::{Order, OrderQuery, OrderStatus};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BackLink, Choice, EmptyState, Field, FlashMap, Html, InputKind, StatusKind,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

const DENIED: &str = "You do not have permission to record goods receipts.";
/// The order statuses goods are received against (Bilimbi
/// `Receiving.receivable_order_statuses/0`).
const RECEIVABLE: [OrderStatus; 2] = [OrderStatus::Sent, OrderStatus::PartiallyReceived];
const LINE_FIELDS: [&str; 3] = ["received_quantity", "rejected_quantity", "rejection_reason"];

#[derive(Template)]
#[template(path = "receive.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    back: Html,
    can_manage: bool,
    picker: Field,
    order: Option<&'a Order>,
    received_on: Field,
    supplier_reference: Field,
    notes: Field,
    lines: Vec<LineRow<'a>>,
    unavailable: Html,
    forbidden: Html,
}
struct LineRow<'a> {
    id: i64,
    number: i32,
    description: &'a str,
    reference: Option<&'a str>,
    ordered: String,
    received: String,
    outstanding: String,
    inputs: [Field; 3],
}

pub(super) struct Receive {
    core: Arc<Core>,
    order_id: Option<i64>,
    order: Option<Order>,
    orders: Vec<Order>,
    can_manage: bool,
    form: HashMap<String, String>,
    rows: HashMap<i64, HashMap<String, String>>,
    flash: Flash,
    redirect: Option<(String, String)>,
}
impl Receive {
    pub(super) fn new(core: Arc<Core>, query: &HashMap<String, String>) -> Self {
        Self {
            core,
            order_id: chosen(query),
            order: None,
            orders: vec![],
            can_manage: false,
            form: HashMap::new(),
            rows: HashMap::new(),
            flash: Flash::default(),
            redirect: None,
        }
    }
    pub(super) async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.can_manage = capabilities(&self.core, scope)
            .await?
            .contains(MANAGE_RECEIPTS);
        let p = &self.core.purchasing;
        let company = company_id(scope);
        // Sent and partially received orders, newest first, as the facade
        // lists one status at a time; an unreadable list is empty.
        let mut orders = vec![];
        for status in RECEIVABLE {
            match p
                .list_orders(
                    scope,
                    company,
                    OrderQuery {
                        status: Some(status),
                        ..OrderQuery::default()
                    },
                )
                .await
            {
                Ok(list) => orders.extend(list),
                Err(Error::Forbidden | Error::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        orders.sort_by_key(|o| std::cmp::Reverse(o.order_number));
        self.orders = orders;
        self.order = match self.order_id {
            Some(id) => match p.get_order(scope, company, id).await {
                Ok(order) => order.filter(|o| RECEIVABLE.contains(&o.status)),
                Err(Error::Forbidden | Error::NotFound) => None,
                Err(e) => return Err(e),
            },
            None => None,
        };
        Ok(())
    }
    async fn event(&mut self, scope: &Scope, payload: &Value) -> Result<()> {
        match text(payload, "type") {
            "clear-flash" => self.flash.clear(payload.get("key").and_then(Value::as_str)),
            "choose" => {
                self.order_id = params::positive_id(Some(text(payload, "picker[order_id]")))
            }
            "save_receipt" => {
                self.can_manage = capabilities(&self.core, scope)
                    .await?
                    .contains(MANAGE_RECEIPTS);
                if !self.can_manage {
                    self.flash.error(DENIED);
                    return Ok(());
                }
                // The order the page showed, as Bilimbi's assigns hold it
                // from the last address change: if it stopped being
                // receivable since, the facade's refusal says why.
                let Some(order_id) = self.order.as_ref().map(|o| o.id) else {
                    self.flash.error("Choose a purchase order first.");
                    return Ok(());
                };
                self.read_form(payload);
                let value = |key: &str| self.form.get(key).cloned().unwrap_or_default();
                let input = NewReceipt {
                    order_id,
                    received_on: Some(value("received_on")),
                    supplier_reference: value("supplier_reference"),
                    notes: value("notes"),
                    lines: self.lines(),
                };
                match outcome(
                    self.core
                        .receiving
                        .create_receipt(scope, company_id(scope), &input)
                        .await,
                )? {
                    Ok(receipt) => {
                        self.redirect = Some((
                            super::address(receipt.id),
                            format!("Goods receipt {} saved as a draft.", receipt.receipt_number),
                        ))
                    }
                    Err(message) => self.flash.error(message),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    /// The header fields and the per-order-line rows the form posted
    /// (`receipt[...]`, `receipt[lines][<order_line_id>][...]`), bounded.
    fn read_form(&mut self, payload: &Value) {
        self.form.clear();
        self.rows.clear();
        let entries = payload
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k, v.as_str().filter(|v| v.len() <= 4096)?)))
            .take(512);
        for (key, value) in entries {
            let Some(inner) = key
                .strip_prefix("receipt[")
                .and_then(|k| k.strip_suffix(']'))
            else {
                continue;
            };
            if let Some(rest) = inner.strip_prefix("lines][") {
                if let Some((id, field)) = rest.split_once("][") {
                    if let (Some(id), true) =
                        (params::positive_id(Some(id)), LINE_FIELDS.contains(&field))
                    {
                        self.rows
                            .entry(id)
                            .or_default()
                            .insert(field.into(), value.into());
                    }
                }
            } else if ["received_on", "supplier_reference", "notes"].contains(&inner) {
                self.form.insert(inner.into(), value.into());
            }
        }
    }
    /// One line per posted row with an accepted or rejected quantity, in
    /// order-line order (Bilimbi `Forms.lines_from_params/1`).
    fn lines(&self) -> Vec<NewReceiptLine> {
        let rows: BTreeMap<_, _> = self.rows.iter().collect();
        rows.into_iter()
            .filter_map(|(id, row)| {
                let v = |field: &str| row.get(field).cloned().unwrap_or_default();
                let (received, rejected) = (v("received_quantity"), v("rejected_quantity"));
                (!received.trim().is_empty() || !rejected.trim().is_empty()).then(|| {
                    NewReceiptLine {
                        order_line_id: *id,
                        received_quantity: received,
                        rejected_quantity: rejected,
                        rejection_reason: v("rejection_reason"),
                    }
                })
            })
            .collect()
    }
    pub(super) fn render(&self) -> Result<String> {
        let value = |key: &str| self.form.get(key).cloned();
        let field = |name: &str, title: &str| {
            Field::in_form("receipt", name)
                .label(title)
                .value(value(name).unwrap_or_default())
        };
        let mut picker = Field::in_form("picker", "order_id")
            .label("Order")
            .kind(InputKind::Select)
            .prompt("Choose a sent order")
            .value(self.order_id.map_or(String::new(), |id| id.to_string()))
            .options(
                self.orders
                    .iter()
                    .map(|o| Choice::new(order_label(o), o.id.to_string()))
                    .collect(),
            );
        if self.orders.is_empty() {
            picker = picker.hint("No sent or partially received order to receive against.");
        }
        let lines = self.order.as_ref().map_or(vec![], |order| {
            order
                .lines
                .iter()
                .map(|line| {
                    let row = self.rows.get(&line.id);
                    LineRow {
                        id: line.id,
                        number: line.line_number,
                        description: &line.description,
                        reference: line.external_item_ref.as_deref(),
                        ordered: format!("{} {}", line.quantity, line.unit),
                        received: line.received_quantity.to_string(),
                        outstanding: (line.quantity - line.received_quantity).to_string(),
                        inputs: LINE_FIELDS.map(|name| {
                            let control =
                                Field::named(format!("receipt[lines][{}][{name}]", line.id))
                                    .id(format!("receipt_lines_{}_{name}", line.id))
                                    .aria_label(format!(
                                        "Line {} {}",
                                        line.line_number,
                                        match name {
                                            "received_quantity" => "accepted quantity",
                                            "rejected_quantity" => "rejected quantity",
                                            _ => "rejection reason",
                                        }
                                    ))
                                    .value(
                                        row.and_then(|r| r.get(name)).cloned().unwrap_or_default(),
                                    );
                            if name == "rejection_reason" {
                                control.attr("maxlength", "500").wrapper_class("w-full")
                            } else {
                                control.attr("inputmode", "decimal").wrapper_class("w-24")
                            }
                        }),
                    }
                })
                .collect()
        });
        View {
            flash: self.flash.map(),
            back: render(&BackLink {
                href: RECEIPTS_PATH,
                title: "Back to goods receipts",
                ..BackLink::default()
            })?,
            can_manage: self.can_manage,
            picker,
            order: self.order.as_ref(),
            received_on: field("received_on", "Received on")
                .kind(InputKind::Date)
                .value(value("received_on").unwrap_or_else(today)),
            supplier_reference: field("supplier_reference", "Delivery note")
                .hint("The supplier's reference for this delivery."),
            notes: field("notes", "Notes").kind(InputKind::Textarea),
            lines,
            unavailable: if self.order_id.is_some() && self.order.is_none() {
                render(
                    &EmptyState::missing(
                        "This purchase order cannot be received against.",
                        "It is not sent or partially received, belongs to another company, or the link is out of date.",
                    )
                    .id("order-unavailable"),
                )?
            } else {
                Html::EMPTY
            },
            forbidden: render(
                &EmptyState::missing(
                    DENIED,
                    "Ask for the receipts capability, or view the receipts list instead.",
                )
                .id("receive-forbidden"),
            )?,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}
impl LiveState<Scope, Error> for Receive {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(q) => self.order_id = chosen(q),
                LiveInput::Event(p) => self.event(request.scope, p).await?,
            }
            self.load(request.scope).await?;
            let (redirect, flash) = self
                .redirect
                .take()
                .map_or((None, None), |(path, message)| {
                    (Some(path), Some((StatusKind::Success, message)))
                });
            Ok(LiveBody {
                html: self.render()?,
                query: self
                    .order_id
                    .map(|id| HashMap::from([("order".into(), id.to_string())]))
                    .unwrap_or_default(),
                redirect,
                flash,
                ..LiveBody::default()
            })
        })
    }
}

fn chosen(query: &HashMap<String, String>) -> Option<i64> {
    params::positive_id(query.get("order").map(String::as_str))
}
fn order_label(order: &Order) -> String {
    format!(
        "PO {} · {} · {}",
        order.order_number,
        order
            .supplier
            .as_ref()
            .map_or("supplier", |s| s.name.as_str()),
        label(order.status.as_str())
    )
}
/// UTC today, the form's default receipt date (Bilimbi `Date.utc_today/0`).
fn today() -> String {
    jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .to_string()
}
