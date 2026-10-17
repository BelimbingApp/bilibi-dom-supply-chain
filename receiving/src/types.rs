//! Schema-free views and inputs of Bilimbi SupplyChain.Receiving.
use rust_decimal::Decimal;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptStatus {
    Draft,
    Posted,
    Cancelled,
}
impl ReceiptStatus {
    pub const ALL: [Self; 3] = [Self::Draft, Self::Posted, Self::Cancelled];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Posted => "posted",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptKind {
    Receipt,
    Reversal,
}
impl ReceiptKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Receipt => "receipt",
            Self::Reversal => "reversal",
        }
    }
}

/// List views include lines but no history. Single reads and writes include
/// history and derived reversal links; posting events do not use those links.
#[derive(Clone, Debug)]
pub struct Receipt {
    pub id: i64,
    pub receipt_number: i64,
    pub kind: ReceiptKind,
    pub status: ReceiptStatus,
    pub order_id: i64,
    pub order_number: i64,
    pub supplier_id: i64,
    pub reverses_receipt_id: Option<i64>,
    pub reason: Option<String>,
    pub received_on: String,
    pub supplier_reference: Option<String>,
    pub notes: Option<String>,
    pub created_by_user_id: i64,
    pub created_by_impersonator_id: Option<i64>,
    pub posted_by_user_id: Option<i64>,
    pub posted_by_impersonator_id: Option<i64>,
    pub posted_at: Option<SystemTime>,
    pub cancelled_at: Option<SystemTime>,
    pub inserted_at: SystemTime,
    pub updated_at: SystemTime,
    pub reverses_receipt_number: Option<i64>,
    pub reversed_by_receipt_id: Option<i64>,
    pub reversed_by_receipt_number: Option<i64>,
    pub lines: Vec<ReceiptLine>,
    pub events: Vec<ReceiptEvent>,
}

#[derive(Clone, Debug)]
pub struct ReceiptLine {
    pub id: i64,
    pub line_number: i32,
    pub order_line_id: i64,
    pub order_line_number: i32,
    pub description: String,
    pub unit: String,
    pub ordered_quantity: Decimal,
    pub received_quantity: Decimal,
    pub rejected_quantity: Decimal,
    pub rejection_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ReceiptEvent {
    pub id: i64,
    pub from_status: Option<ReceiptStatus>,
    pub to_status: ReceiptStatus,
    pub actor_user_id: i64,
    pub actor_impersonator_id: Option<i64>,
    pub reason: Option<String>,
    pub occurred_at: SystemTime,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub over_receipt_tolerance_percent: Decimal,
    pub set_by_user_id: Option<i64>,
    pub set_by_impersonator_id: Option<i64>,
    pub occurred_at: Option<SystemTime>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            over_receipt_tolerance_percent: Decimal::ZERO,
            set_by_user_id: None,
            set_by_impersonator_id: None,
            occurred_at: None,
        }
    }
}

/// Quantities are decimal text, defaulting to zero when blank. At least one
/// quantity must be positive; rejected goods require a reason.
#[derive(Clone, Debug, Default)]
pub struct NewReceiptLine {
    pub order_line_id: i64,
    pub received_quantity: String,
    pub rejected_quantity: String,
    pub rejection_reason: String,
}

/// `received_on: None` means UTC today; Some(blank) is invalid.
#[derive(Clone, Debug, Default)]
pub struct NewReceipt {
    pub order_id: i64,
    pub received_on: Option<String>,
    pub supplier_reference: String,
    pub notes: String,
    pub lines: Vec<NewReceiptLine>,
}

/// Omitted fields stay unchanged; blank optional texts clear them. Given
/// lines replace the whole set. Header-only edits do not read Purchasing.
#[derive(Clone, Debug, Default)]
pub struct ReceiptChanges {
    pub received_on: Option<String>,
    pub supplier_reference: Option<String>,
    pub notes: Option<String>,
    pub lines: Option<Vec<NewReceiptLine>>,
}

#[derive(Clone, Debug, Default)]
pub struct Reversal {
    pub reason: String,
    pub received_on: Option<String>,
    pub notes: String,
}

#[derive(Clone, Copy, Debug)]
pub struct ReceiptQuery {
    pub status: Option<ReceiptStatus>,
    pub order_id: Option<i64>,
    pub limit: i64,
}
impl Default for ReceiptQuery {
    fn default() -> Self {
        Self {
            status: None,
            order_id: None,
            limit: 100,
        }
    }
}

/// An omitted cursor starts at zero; an explicitly given cursor is positive.
#[derive(Clone, Copy, Debug)]
pub struct PostingQuery {
    pub after_posting_id: Option<i64>,
    pub limit: i64,
}
impl Default for PostingQuery {
    fn default() -> Self {
        Self {
            after_posting_id: None,
            limit: 100,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Posting {
    pub posting_id: i64,
    pub posted_at: SystemTime,
    pub receipt: Receipt,
}

#[derive(Clone, Debug)]
pub struct LineReceipt {
    pub receipt_id: i64,
    pub receipt_number: i64,
    pub kind: ReceiptKind,
    pub reverses_receipt_id: Option<i64>,
    pub received_on: String,
    pub posted_at: SystemTime,
    pub received_quantity: Decimal,
    pub rejected_quantity: Decimal,
    pub rejection_reason: Option<String>,
}

/// Posted net figures, including superseded order lines. No matches return
/// the supplied line ID, null snapshot facts, zeros and an empty receipt list.
#[derive(Clone, Debug)]
pub struct OrderLineReceipts {
    pub order_line_id: i64,
    pub order_id: Option<i64>,
    pub order_number: Option<i64>,
    pub order_line_number: Option<i32>,
    pub description: Option<String>,
    pub unit: Option<String>,
    pub ordered_quantity: Option<Decimal>,
    pub received_quantity: Decimal,
    pub rejected_quantity: Decimal,
    pub receipts: Vec<LineReceipt>,
}
impl OrderLineReceipts {
    pub(crate) fn empty(order_line_id: i64) -> Self {
        Self {
            order_line_id,
            order_id: None,
            order_number: None,
            order_line_number: None,
            description: None,
            unit: None,
            ordered_quantity: None,
            received_quantity: Decimal::ZERO,
            rejected_quantity: Decimal::ZERO,
            receipts: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub line: Option<usize>,
    pub message: String,
}
impl FieldError {
    pub(crate) fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            line: None,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Invalid(Vec<FieldError>),
    ReceiptNotFound,
    OrderNotFound,
    NoLines,
    ReceiptReversed,
    ReversalNotReversible,
    OrderNotReceivable(&'static str),
    InvalidTransition {
        status: &'static str,
        target: &'static str,
    },
    Purchasing(bilibi_supply_chain_purchasing::Refusal),
}
impl Refusal {
    /// Bilimbi Receiving.Web.Refusals, kept out of pages.
    pub fn text(&self) -> String {
        match self {
            Self::Invalid(errors) if errors.is_empty() => "Check the values entered.".into(),
            Self::Invalid(errors) => errors
                .iter()
                .map(|e| {
                    let field = e.field.replace('_', " ");
                    let label = if e.line.is_some() {
                        format!("Lines {field}")
                    } else {
                        let mut chars = field.chars();
                        chars
                            .next()
                            .map(|c| c.to_uppercase().chain(chars).collect())
                            .unwrap_or_default()
                    };
                    format!("{label} {}.", e.message)
                })
                .collect::<Vec<_>>()
                .join(" "),
            Self::ReceiptNotFound => "That goods receipt is no longer available.".into(),
            Self::OrderNotFound => "That purchase order is no longer available.".into(),
            Self::NoLines => "Enter a received or rejected quantity on at least one line.".into(),
            Self::ReceiptReversed => "This receipt was already reversed.".into(),
            Self::ReversalNotReversible => {
                "A reversal is not reversed; record the goods again with a new receipt.".into()
            }
            Self::OrderNotReceivable(status) => format!(
                "This purchase order is {}; goods are received against a sent order.",
                status.replace('_', " ")
            ),
            Self::InvalidTransition { status, .. } => format!(
                "This receipt is {}, so that action no longer applies.",
                status.replace('_', " ")
            ),
            Self::Purchasing(reason) => reason.text(),
        }
    }
}

#[derive(Debug)]
pub enum Outcome<T> {
    Saved(T),
    Refused(Refusal),
    CompanyArchived,
}
