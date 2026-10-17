//! The purchase order records and inputs the facade exchanges (Bilimbi's
//! `Order`, `OrderLine`, `OrderEvent` and `OrderAmendment` views). Re-exported
//! from the crate root; the schema behind them stays private.
use crate::{Supplier, DEFAULT_LIMIT};
use rust_decimal::Decimal;
use std::time::SystemTime;

/// The statuses a purchase order moves through: the one fixed
/// classification, persisted under these names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderStatus {
    Draft,
    Submitted,
    Approved,
    Sent,
    PartiallyReceived,
    Closed,
    Cancelled,
}
impl OrderStatus {
    pub const ALL: [Self; 7] = [
        Self::Draft,
        Self::Submitted,
        Self::Approved,
        Self::Sent,
        Self::PartiallyReceived,
        Self::Closed,
        Self::Cancelled,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Submitted => "submitted",
            Self::Approved => "approved",
            Self::Sent => "sent",
            Self::PartiallyReceived => "partially_received",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == value)
    }
}

/// A purchase order: its header, the supplier it names, the number of the
/// request it was ordered from, its current lines with totals, and (from
/// [`crate::Purchasing::get_order`]) its status history and amendments, oldest
/// first. `expected_on` is an ISO date (`YYYY-MM-DD`). Totals are the sums
/// of the current lines' `line_total` and `tax_amount`.
#[derive(Clone, Debug)]
pub struct Order {
    pub id: i64,
    pub order_number: i64,
    pub status: OrderStatus,
    pub purchase_request_id: Option<i64>,
    pub purchase_request_number: Option<i64>,
    pub supplier_id: i64,
    pub supplier: Option<Supplier>,
    pub currency_code: String,
    pub reference: Option<String>,
    pub notes: Option<String>,
    pub expected_on: Option<String>,
    pub amendment_count: i32,
    pub created_by_user_id: i64,
    pub created_by_impersonator_id: Option<i64>,
    pub submitted_by_user_id: Option<i64>,
    pub submitted_at: Option<SystemTime>,
    pub approved_by_user_id: Option<i64>,
    pub approved_at: Option<SystemTime>,
    pub sent_at: Option<SystemTime>,
    pub closed_at: Option<SystemTime>,
    pub cancelled_at: Option<SystemTime>,
    pub inserted_at: SystemTime,
    pub updated_at: SystemTime,
    pub lines: Vec<OrderLine>,
    pub subtotal: Decimal,
    pub tax_total: Decimal,
    pub total: Decimal,
    pub events: Vec<OrderEvent>,
    pub amendments: Vec<Amendment>,
}

/// One order line: what is bought, how much, in which unit, at what price,
/// with an optional tax code whose rate the line snapshotted when written,
/// and how much has been received. `amendment_id` is the amendment that added
/// it; `superseded_by_amendment_id` the one that replaced it. `line_total` is
/// quantity times unit price, `tax_amount` that total times the rate.
#[derive(Clone, Debug)]
pub struct OrderLine {
    pub id: i64,
    pub line_number: i32,
    pub description: String,
    pub external_item_ref: Option<String>,
    pub quantity: Decimal,
    pub unit: String,
    pub unit_price: Decimal,
    pub tax_code_id: Option<i64>,
    pub tax_rate: Option<Decimal>,
    pub received_quantity: Decimal,
    pub amendment_id: Option<i64>,
    pub superseded_by_amendment_id: Option<i64>,
    pub line_total: Decimal,
    pub tax_amount: Decimal,
}

/// One append-only history row of an order, as [`crate::RequestEvent`] is
/// for a request.
#[derive(Clone, Debug)]
pub struct OrderEvent {
    pub id: i64,
    pub from_status: Option<OrderStatus>,
    pub to_status: OrderStatus,
    pub actor_user_id: i64,
    pub actor_impersonator_id: Option<i64>,
    pub reason: Option<String>,
    pub occurred_at: SystemTime,
}

/// One amendment of an approved or sent order: its number, reason and
/// author, the lines it added and the IDs of the lines it superseded.
#[derive(Clone, Debug)]
pub struct Amendment {
    pub id: i64,
    pub amendment_number: i32,
    pub reason: String,
    pub amended_by_user_id: i64,
    pub amended_by_impersonator_id: Option<i64>,
    pub occurred_at: SystemTime,
    pub lines: Vec<OrderLine>,
    pub superseded_line_ids: Vec<i64>,
}

/// One current or superseded order line with the facts another module
/// matches against (a supplier's bill line, a goods receipt line): the line
/// and its order's ID, number, status, supplier and currency.
#[derive(Clone, Debug)]
pub struct OrderLineFacts {
    pub line: OrderLine,
    pub order_id: i64,
    pub order_number: i64,
    pub order_status: OrderStatus,
    pub supplier_id: i64,
    pub currency_code: String,
}

/// An order as a form submits it: texts as typed, validated by the facade. A
/// draft may have no lines yet; submission needs at least one.
#[derive(Clone, Debug, Default)]
pub struct NewOrder {
    pub supplier_id: Option<i64>,
    pub currency_code: String,
    pub reference: String,
    pub notes: String,
    pub expected_on: String,
    pub lines: Vec<NewOrderLine>,
}

/// An order line as a form submits it; `quantity` and `unit_price` are the
/// texts typed, and `tax_code_id` names an active tax code of the company.
#[derive(Clone, Debug, Default)]
pub struct NewOrderLine {
    pub description: String,
    pub external_item_ref: String,
    pub quantity: String,
    pub unit: String,
    pub unit_price: String,
    pub tax_code_id: Option<i64>,
}

/// A draft order made from an approved request: the header as for
/// [`NewOrder`], and `lines`, or `None` to copy the request's lines with a
/// unit price of zero, to be priced on the draft.
#[derive(Clone, Debug, Default)]
pub struct OrderFromRequest {
    pub supplier_id: Option<i64>,
    pub currency_code: String,
    pub reference: String,
    pub notes: String,
    pub expected_on: String,
    pub lines: Option<Vec<NewOrderLine>>,
}

/// A change to a draft order: only the fields given change, and `lines`,
/// when given, replaces every line.
#[derive(Clone, Debug, Default)]
pub struct OrderChanges {
    pub supplier_id: Option<i64>,
    pub currency_code: Option<String>,
    pub reference: Option<String>,
    pub notes: Option<String>,
    pub expected_on: Option<String>,
    pub lines: Option<Vec<NewOrderLine>>,
}

/// An amendment of an approved or sent order: a `reason` (required), the
/// lines that replace the current ones (at least one), and any `reference`,
/// `notes` or `expected_on` given (blank clears it).
#[derive(Clone, Debug, Default)]
pub struct OrderAmendment {
    pub reason: String,
    pub reference: Option<String>,
    pub notes: Option<String>,
    pub expected_on: Option<String>,
    pub lines: Vec<NewOrderLine>,
}

/// Goods received against, or taken back from, one current line of an order.
#[derive(Clone, Copy, Debug)]
pub struct ReceiptLine {
    pub line_id: i64,
    pub quantity: Decimal,
}

/// Which orders [`crate::Purchasing::list_orders`] returns: one status or all,
/// one supplier or all, and at most `limit` (1 to [`crate::MAX_LIMIT`],
/// default [`DEFAULT_LIMIT`]).
#[derive(Clone, Copy, Debug)]
pub struct OrderQuery {
    pub status: Option<OrderStatus>,
    pub supplier_id: Option<i64>,
    pub limit: i64,
}
impl Default for OrderQuery {
    fn default() -> Self {
        Self {
            status: None,
            supplier_id: None,
            limit: DEFAULT_LIMIT,
        }
    }
}
