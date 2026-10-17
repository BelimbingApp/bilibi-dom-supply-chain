//! Purchasing (`supply_chain/purchasing`): a company's suppliers and tax
//! codes, its purchase requests, and its purchase orders with their
//! amendments and the goods received against them, and the Suppliers and
//! Purchase requests pages.
//!
//! This is the module's one facade. Other modules read and write through
//! [`Purchasing`], resolved from the registry as [`SERVICE`]; the schema, SQL
//! and validation stay private. Every call takes the sealed
//! [`Scope`] it was handed and an explicit company ID. Reads authorize
//! [`VIEW`] and writes their own capability for the scope's actor and the
//! target company at the moment they run (Core Company's
//! `authorize_target`: the actor's own company, or a sibling company with
//! tenant-wide reach), so a missing grant is `Error::Forbidden` and a
//! company the scope cannot see is `Error::NotFound`. A write then holds the
//! company's writable lock inside its audited mutation transaction, so an
//! archived company is [`Outcome::CompanyArchived`] and one company's
//! purchasing writes run one at a time. See `README.md`.
mod agent_operations;
mod contributions;
mod order_changes;
mod order_lifecycle;
mod order_types;
mod orders;
mod port;
mod requests;
mod suppliers;
mod tax_codes;
mod validate;
mod web;

pub use contributions::{
    APPROVE_ORDERS, APPROVE_REQUESTS, CAPABILITIES, CONFIGURE, MANAGE_ORDERS, MANAGE_REQUESTS,
    RECEIVE_ORDERS, VIEW,
};
pub use order_types::{
    Amendment, NewOrder, NewOrderLine, Order, OrderAmendment, OrderChanges, OrderEvent,
    OrderFromRequest, OrderLine, OrderLineFacts, OrderQuery, OrderStatus, ReceiptLine,
};

use bilibi_company::{organisation, WriteError};
use bilibi_database::{Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use rust_decimal::Decimal;
use std::{sync::Arc, time::SystemTime};
use tokio_postgres::{Client, GenericClient};

/// The module ID, Bilimbi's, kept because IDs are persisted data.
pub const MODULE: &str = "supply_chain/purchasing";
/// The registry service ID of the [`Purchasing`] facade.
pub const SERVICE: &str = "supply_chain/purchasing.purchasing";

/// One refusal of a submitted value, in the words a form shows beside the
/// field. `line` is the 1-based line of a request's lines the field belongs
/// to, or `None` for a header field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub line: Option<usize>,
    pub message: String,
}
impl FieldError {
    pub fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            line: None,
            message: message.into(),
        }
    }
}

/// Why a write was refused (Bilimbi's `{:error, reason}` short of
/// authorization): every case a page names to the reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The input broke a rule; one message per field (an invalid changeset).
    Invalid(Vec<FieldError>),
    SupplierNotFound,
    /// The order names a supplier that has been deactivated.
    SupplierInactive,
    TaxCodeNotFound,
    RequestNotFound,
    OrderNotFound,
    /// Goods are received against the order, so it is not cancelled.
    GoodsReceived,
    /// The record is in `status`, from which the action toward `target`
    /// does not apply (`{:invalid_transition, status, target}`).
    InvalidTransition {
        status: &'static str,
        target: &'static str,
    },
    /// Submission needs at least one line.
    NoLines,
    /// The approver or rejecter is the person who submitted the request.
    OwnRequest,
    /// The approver is the person who submitted (or amended) the order.
    OwnOrder,
}
impl Refusal {
    /// The reader-facing sentence, as Bilimbi's `Web.Refusals.text/1` words it.
    pub fn text(&self) -> String {
        match self {
            Self::Invalid(errors) if errors.is_empty() => "Check the values entered.".into(),
            Self::Invalid(errors) => errors
                .iter()
                .map(|error| {
                    let field = error.field.replace('_', " ");
                    let label = match error.line {
                        Some(_) => format!("Lines {field}"),
                        None => capitalize(&field),
                    };
                    format!("{label} {}.", error.message)
                })
                .collect::<Vec<_>>()
                .join(" "),
            Self::SupplierNotFound => "That supplier is no longer available.".into(),
            Self::SupplierInactive => "Choose an active supplier.".into(),
            Self::TaxCodeNotFound => "That tax code is no longer available.".into(),
            Self::RequestNotFound => "That purchase request is no longer available.".into(),
            Self::OrderNotFound => "That purchase order is no longer available.".into(),
            Self::GoodsReceived => {
                "Goods are received against this order. Reverse the receipts, or close it instead."
                    .into()
            }
            Self::InvalidTransition { status, .. } => format!(
                "This record is {}, so that action no longer applies.",
                status.replace('_', " ")
            ),
            Self::NoLines => "Add at least one line first.".into(),
            Self::OwnRequest => "You submitted this request, so someone else decides it.".into(),
            Self::OwnOrder => "You submitted this order, so someone else approves it.".into(),
        }
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// How a write ended. A refusal is an outcome a page names, never an `Error`.
#[derive(Debug)]
pub enum Outcome<T> {
    Saved(T),
    /// The write broke a rule or no longer applies; nothing changed.
    Refused(Refusal),
    /// The company is archived and read-only (Bilimbi `{:error, :company_archived}`).
    CompanyArchived,
}

/// A party the company buys from: a code unique in the company and fixed for
/// good, a name, and optional contact and address facts. Deactivated, never
/// deleted.
#[derive(Clone, Debug)]
pub struct Supplier {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub contact_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub address_line3: Option<String>,
    pub locality: Option<String>,
    pub postcode: Option<String>,
    pub country_iso: Option<String>,
    pub admin1_code: Option<String>,
    pub active: bool,
    pub inserted_at: SystemTime,
    pub updated_at: SystemTime,
}

/// A supplier as a form submits it: texts are trimmed, and a blank optional
/// text is stored as absent. `active` defaults to true, as the schema does.
#[derive(Clone, Debug)]
pub struct NewSupplier {
    pub code: String,
    pub name: String,
    pub contact_name: String,
    pub email: String,
    pub phone: String,
    pub address_line1: String,
    pub address_line2: String,
    pub address_line3: String,
    pub locality: String,
    pub postcode: String,
    pub country_iso: String,
    pub admin1_code: String,
    pub active: bool,
}
impl Default for NewSupplier {
    fn default() -> Self {
        Self {
            code: String::new(),
            name: String::new(),
            contact_name: String::new(),
            email: String::new(),
            phone: String::new(),
            address_line1: String::new(),
            address_line2: String::new(),
            address_line3: String::new(),
            locality: String::new(),
            postcode: String::new(),
            country_iso: String::new(),
            admin1_code: String::new(),
            active: true,
        }
    }
}

/// A change to a supplier's details or `active` flag (Bilimbi
/// `update_changeset/2`): only the fields given change, and its code never
/// does. A blank optional text clears it; the name stays required.
#[derive(Clone, Debug, Default)]
pub struct SupplierChanges {
    pub name: Option<String>,
    pub contact_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub address_line3: Option<String>,
    pub locality: Option<String>,
    pub postcode: Option<String>,
    pub country_iso: Option<String>,
    pub admin1_code: Option<String>,
    pub active: Option<bool>,
}

/// A company's tax code: a code fixed for good, a name, and a rate in
/// percent with up to four decimal places.
#[derive(Clone, Debug)]
pub struct TaxCode {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub rate: Decimal,
    pub active: bool,
}

/// A tax code as a form submits it; `rate` is the text typed, validated here.
#[derive(Clone, Debug)]
pub struct NewTaxCode {
    pub code: String,
    pub name: String,
    pub rate: String,
    pub active: bool,
}
impl Default for NewTaxCode {
    fn default() -> Self {
        Self {
            code: String::new(),
            name: String::new(),
            rate: String::new(),
            active: true,
        }
    }
}

/// A change to a tax code's `name`, `rate` or `active` flag; only the fields
/// given change, and its code never does. Lines already written keep the
/// rate they snapshotted.
#[derive(Clone, Debug, Default)]
pub struct TaxCodeChanges {
    pub name: Option<String>,
    pub rate: Option<String>,
    pub active: Option<bool>,
}

/// The statuses a purchase request moves through: the one fixed
/// classification, persisted under these names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestStatus {
    Draft,
    Submitted,
    Approved,
    Rejected,
    Cancelled,
}
impl RequestStatus {
    pub const ALL: [Self; 5] = [
        Self::Draft,
        Self::Submitted,
        Self::Approved,
        Self::Rejected,
        Self::Cancelled,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Submitted => "submitted",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == value)
    }
}

/// A purchase request: a numbered ask to buy, its lines, and (from
/// [`Purchasing::get_request`]) its status history, oldest event first.
/// `needed_on` is an ISO date (`YYYY-MM-DD`).
#[derive(Clone, Debug)]
pub struct Request {
    pub id: i64,
    pub request_number: i64,
    pub status: RequestStatus,
    pub title: String,
    pub justification: Option<String>,
    pub needed_on: Option<String>,
    pub created_by_user_id: i64,
    pub created_by_impersonator_id: Option<i64>,
    pub submitted_by_user_id: Option<i64>,
    pub submitted_at: Option<SystemTime>,
    pub decided_by_user_id: Option<i64>,
    pub decided_at: Option<SystemTime>,
    pub decision_reason: Option<String>,
    pub inserted_at: SystemTime,
    pub updated_at: SystemTime,
    pub lines: Vec<RequestLine>,
    pub events: Vec<RequestEvent>,
}

/// One line of a request: what is needed, how much, and in which unit. The
/// item is described as text with an optional free reference into any
/// catalogue; Purchasing never resolves it.
#[derive(Clone, Debug)]
pub struct RequestLine {
    pub id: i64,
    pub line_number: i32,
    pub description: String,
    pub external_item_ref: Option<String>,
    pub quantity: Decimal,
    pub unit: String,
}

/// One append-only history row: the creation (`from_status` absent) or a
/// transition, with its actor, any impersonator, the reason given and the
/// time the database recorded it.
#[derive(Clone, Debug)]
pub struct RequestEvent {
    pub id: i64,
    pub from_status: Option<RequestStatus>,
    pub to_status: RequestStatus,
    pub actor_user_id: i64,
    pub actor_impersonator_id: Option<i64>,
    pub reason: Option<String>,
    pub occurred_at: SystemTime,
}

/// A request as a form submits it; a draft may have no lines yet.
#[derive(Clone, Debug, Default)]
pub struct NewRequest {
    pub title: String,
    pub justification: String,
    pub needed_on: String,
    pub lines: Vec<NewRequestLine>,
}

/// A request line as a form submits it; `quantity` is the text typed.
#[derive(Clone, Debug, Default)]
pub struct NewRequestLine {
    pub description: String,
    pub external_item_ref: String,
    pub quantity: String,
    pub unit: String,
}

/// A change to a draft request: only the fields given change, and `lines`,
/// when given, replaces every line.
#[derive(Clone, Debug, Default)]
pub struct RequestChanges {
    pub title: Option<String>,
    pub justification: Option<String>,
    pub needed_on: Option<String>,
    pub lines: Option<Vec<NewRequestLine>>,
}

/// Which requests [`Purchasing::list_requests`] returns: one status or all,
/// and at most `limit` (1 to [`MAX_LIMIT`], default [`DEFAULT_LIMIT`]).
#[derive(Clone, Copy, Debug)]
pub struct RequestQuery {
    pub status: Option<RequestStatus>,
    pub limit: i64,
}
impl Default for RequestQuery {
    fn default() -> Self {
        Self {
            status: None,
            limit: DEFAULT_LIMIT,
        }
    }
}

/// The rows a list returns unless asked otherwise, and the most it returns.
pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 500;

/// Bilimbi `write/4`: [`Purchasing::begin`], then `$body` (an async write
/// on `&mut $tx` answering an [`Outcome`]), committed when saved and rolled
/// back otherwise.
macro_rules! write {
    ($self:ident, $scope:expr, $company_id:expr, $capability:expr, $action:literal, |$tx:ident| $body:expr) => {{
        let mut client = $self.db.get().await?;
        let Some(mut $tx) = $self
            .begin(&mut client, $scope, $company_id, $capability, $action)
            .await?
        else {
            return Ok(Outcome::CompanyArchived);
        };
        let outcome = $body.await?;
        finish($tx, &outcome).await?;
        Ok(outcome)
    }};
}

/// The facade (Bilimbi `Bilimbi.SupplyChain.Purchasing`).
pub struct Purchasing {
    db: Db,
    mutations: Arc<MutationBoundary>,
}
impl Purchasing {
    pub(crate) fn new(db: Db, mutations: Arc<MutationBoundary>) -> Self {
        Self { db, mutations }
    }

    /// The company's suppliers ordered by code; `active_only` keeps the active
    /// ones. Capability [`VIEW`].
    pub async fn list_suppliers(
        &self,
        scope: &Scope,
        company_id: i64,
        active_only: bool,
    ) -> Result<Vec<Supplier>> {
        let client = self.db.get().await?;
        authorize(&**client, scope, company_id, VIEW).await?;
        suppliers::list(&**client, scope.tenant(), company_id, active_only).await
    }

    /// Adds a supplier: `code` (unique in the company, fixed for good), `name`,
    /// and optional contact and address facts. Capability [`CONFIGURE`].
    pub async fn create_supplier(
        &self,
        scope: &Scope,
        company_id: i64,
        supplier: &NewSupplier,
    ) -> Result<Outcome<Supplier>> {
        write!(
            self,
            scope,
            company_id,
            CONFIGURE,
            "supply_chain.purchasing.supplier.create",
            |tx| suppliers::create(&mut tx, scope.tenant(), company_id, supplier)
        )
    }

    /// One supplier of the company. Capability [`VIEW`].
    pub async fn get_supplier(
        &self,
        scope: &Scope,
        company_id: i64,
        supplier_id: i64,
    ) -> Result<Option<Supplier>> {
        let client = self.db.get().await?;
        authorize(&**client, scope, company_id, VIEW).await?;
        suppliers::get(&**client, scope.tenant(), company_id, supplier_id).await
    }

    /// Changes a supplier's details or `active` flag; its code never changes.
    /// An inactive supplier takes no new orders; its orders and history stay.
    /// Capability [`CONFIGURE`].
    pub async fn update_supplier(
        &self,
        scope: &Scope,
        company_id: i64,
        supplier_id: i64,
        changes: &SupplierChanges,
    ) -> Result<Outcome<Supplier>> {
        write!(
            self,
            scope,
            company_id,
            CONFIGURE,
            "supply_chain.purchasing.supplier.update",
            |tx| suppliers::update(&mut tx, scope.tenant(), company_id, supplier_id, changes)
        )
    }

    /// The company's tax codes ordered by code; `active_only` keeps the active
    /// ones. Capability [`VIEW`].
    pub async fn list_tax_codes(
        &self,
        scope: &Scope,
        company_id: i64,
        active_only: bool,
    ) -> Result<Vec<TaxCode>> {
        let client = self.db.get().await?;
        authorize(&**client, scope, company_id, VIEW).await?;
        tax_codes::list(&**client, scope.tenant(), company_id, active_only).await
    }

    /// Adds a tax code: `code` (unique in the company, fixed for good), `name`
    /// and `rate`, a percentage from 0 to 100 with up to four decimal places.
    /// Capability [`CONFIGURE`].
    pub async fn create_tax_code(
        &self,
        scope: &Scope,
        company_id: i64,
        tax_code: &NewTaxCode,
    ) -> Result<Outcome<TaxCode>> {
        write!(
            self,
            scope,
            company_id,
            CONFIGURE,
            "supply_chain.purchasing.tax_code.create",
            |tx| tax_codes::create(&mut tx, scope.tenant(), company_id, tax_code)
        )
    }

    /// Changes a tax code's `name`, `rate` or `active` flag; its code never
    /// changes, and lines already written keep the rate they snapshotted.
    /// Capability [`CONFIGURE`].
    pub async fn update_tax_code(
        &self,
        scope: &Scope,
        company_id: i64,
        tax_code_id: i64,
        changes: &TaxCodeChanges,
    ) -> Result<Outcome<TaxCode>> {
        write!(
            self,
            scope,
            company_id,
            CONFIGURE,
            "supply_chain.purchasing.tax_code.update",
            |tx| tax_codes::update(&mut tx, scope.tenant(), company_id, tax_code_id, changes)
        )
    }

    /// The company's purchase requests with their lines (no history), newest
    /// number first. A `limit` outside 1 to [`MAX_LIMIT`] is
    /// `Error::Invalid`. Capability [`VIEW`].
    pub async fn list_requests(
        &self,
        scope: &Scope,
        company_id: i64,
        query: RequestQuery,
    ) -> Result<Vec<Request>> {
        if !(1..=MAX_LIMIT).contains(&query.limit) {
            return Err(Error::Invalid);
        }
        let client = self.db.get().await?;
        authorize(&**client, scope, company_id, VIEW).await?;
        requests::list(&**client, scope.tenant(), company_id, query).await
    }

    /// One purchase request with its lines and its status history, oldest
    /// event first. Capability [`VIEW`].
    pub async fn get_request(
        &self,
        scope: &Scope,
        company_id: i64,
        request_id: i64,
    ) -> Result<Option<Request>> {
        let client = self.db.get().await?;
        authorize(&**client, scope, company_id, VIEW).await?;
        requests::get(&**client, scope.tenant(), company_id, request_id).await
    }

    /// Creates a draft purchase request: `title`, optional `justification`
    /// and `needed_on`, and lines, each a description, quantity and unit
    /// with an optional external item reference. A draft may have no lines
    /// yet; submission needs at least one. Capability [`MANAGE_REQUESTS`].
    pub async fn create_request(
        &self,
        scope: &Scope,
        company_id: i64,
        request: &NewRequest,
    ) -> Result<Outcome<Request>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_REQUESTS,
            "supply_chain.purchasing.request.create",
            |tx| requests::create(&mut tx, scope, company_id, request)
        )
    }

    /// Changes a draft request's title, justification or date, and replaces
    /// its lines when `lines` is given. A request that is no longer a draft
    /// is refused as an invalid transition. Capability [`MANAGE_REQUESTS`].
    pub async fn update_request(
        &self,
        scope: &Scope,
        company_id: i64,
        request_id: i64,
        changes: &RequestChanges,
    ) -> Result<Outcome<Request>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_REQUESTS,
            "supply_chain.purchasing.request.update",
            |tx| requests::update(&mut tx, scope, company_id, request_id, changes)
        )
    }

    /// Submits a draft request for approval; it needs at least one line.
    /// Capability [`MANAGE_REQUESTS`].
    pub async fn submit_request(
        &self,
        scope: &Scope,
        company_id: i64,
        request_id: i64,
    ) -> Result<Outcome<Request>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_REQUESTS,
            "supply_chain.purchasing.request.submit",
            |tx| requests::submit(&mut tx, scope, company_id, request_id)
        )
    }

    /// Approves a submitted request, with an optional `reason` (blank for
    /// none). The approver is not the person who submitted it. Capability
    /// [`APPROVE_REQUESTS`].
    pub async fn approve_request(
        &self,
        scope: &Scope,
        company_id: i64,
        request_id: i64,
        reason: &str,
    ) -> Result<Outcome<Request>> {
        write!(
            self,
            scope,
            company_id,
            APPROVE_REQUESTS,
            "supply_chain.purchasing.request.approve",
            |tx| requests::decide(
                &mut tx,
                scope,
                company_id,
                request_id,
                RequestStatus::Approved,
                reason
            )
        )
    }

    /// Rejects a submitted request; a `reason` is required and the requester
    /// sees it. The rejecter is not the person who submitted it. Capability
    /// [`APPROVE_REQUESTS`].
    pub async fn reject_request(
        &self,
        scope: &Scope,
        company_id: i64,
        request_id: i64,
        reason: &str,
    ) -> Result<Outcome<Request>> {
        write!(
            self,
            scope,
            company_id,
            APPROVE_REQUESTS,
            "supply_chain.purchasing.request.reject",
            |tx| requests::decide(
                &mut tx,
                scope,
                company_id,
                request_id,
                RequestStatus::Rejected,
                reason
            )
        )
    }

    /// Cancels a draft, submitted or approved request; a `reason` is
    /// required. Capability [`MANAGE_REQUESTS`].
    pub async fn cancel_request(
        &self,
        scope: &Scope,
        company_id: i64,
        request_id: i64,
        reason: &str,
    ) -> Result<Outcome<Request>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_REQUESTS,
            "supply_chain.purchasing.request.cancel",
            |tx| requests::cancel(&mut tx, scope, company_id, request_id, reason)
        )
    }

    /// Creates a draft purchase order from an approved request: the header
    /// as for [`Purchasing::create_order`] and `lines`, or `None` to copy the
    /// request's lines at a unit price of zero, to be priced on the draft.
    /// The order records the request; a request may be ordered more than once
    /// (from two suppliers, say). Capability [`MANAGE_ORDERS`].
    pub async fn create_order_from_request(
        &self,
        scope: &Scope,
        company_id: i64,
        request_id: i64,
        order: &OrderFromRequest,
    ) -> Result<Outcome<Order>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_ORDERS,
            "supply_chain.purchasing.order.create_from_request",
            |tx| order_lifecycle::create_from_request(
                &mut tx, scope, company_id, request_id, order
            )
        )
    }

    /// The company's purchase orders with their current lines and totals (no
    /// history), newest number first. A `limit` outside 1 to [`MAX_LIMIT`] or
    /// a `supplier_id` below 1 is `Error::Invalid`. Capability [`VIEW`].
    pub async fn list_orders(
        &self,
        scope: &Scope,
        company_id: i64,
        query: OrderQuery,
    ) -> Result<Vec<Order>> {
        if !(1..=MAX_LIMIT).contains(&query.limit) || query.supplier_id.is_some_and(|id| id < 1) {
            return Err(Error::Invalid);
        }
        let client = self.db.get().await?;
        authorize(&**client, scope, company_id, VIEW).await?;
        orders::list(&**client, scope.tenant(), company_id, query).await
    }

    /// One purchase order: its header and supplier, current lines with
    /// totals, status history and amendments (oldest first), each amendment
    /// with the lines it added and the IDs of those it superseded. Capability
    /// [`VIEW`].
    pub async fn get_order(
        &self,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
    ) -> Result<Option<Order>> {
        let client = self.db.get().await?;
        self.get_order_on(&**client, scope, company_id, order_id)
            .await
    }

    /// The same authorized read on the caller's client, including its
    /// uncommitted writes. Receiving uses this inside its company lock.
    pub async fn get_order_on(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
    ) -> Result<Option<Order>> {
        authorize(client, scope, company_id, VIEW).await?;
        orders::get(client, scope.tenant(), company_id, order_id).await
    }

    /// The order with this `order_number` in the company, as
    /// [`Purchasing::get_order`] returns it. Capability [`VIEW`].
    pub async fn find_order_by_number(
        &self,
        scope: &Scope,
        company_id: i64,
        order_number: i64,
    ) -> Result<Option<Order>> {
        let client = self.db.get().await?;
        authorize(&**client, scope, company_id, VIEW).await?;
        orders::find_by_number(&**client, scope.tenant(), company_id, order_number).await
    }

    /// One current or superseded order line with the facts another module
    /// matches against: the line and its order's ID, number, status, supplier
    /// and currency. Capability [`VIEW`]. The same read answers the
    /// `supply_chain/purchasing.order_line` port on the caller's client.
    pub async fn get_order_line(
        &self,
        scope: &Scope,
        company_id: i64,
        line_id: i64,
    ) -> Result<Option<OrderLineFacts>> {
        let client = self.db.get().await?;
        order_line(&**client, scope, company_id, line_id).await
    }

    /// Creates a draft purchase order: `supplier_id` (an active supplier of
    /// the company), `currency_code` (three letters, stored upper-cased),
    /// optional `reference`, `notes` and `expected_on`, and lines, each a
    /// description, quantity, unit and unit price with an optional external
    /// item reference and tax code (an active one, whose rate the line
    /// snapshots). A draft may have no lines yet. Capability
    /// [`MANAGE_ORDERS`].
    pub async fn create_order(
        &self,
        scope: &Scope,
        company_id: i64,
        order: &NewOrder,
    ) -> Result<Outcome<Order>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_ORDERS,
            "supply_chain.purchasing.order.create",
            |tx| order_lifecycle::create(&mut tx, scope, company_id, order)
        )
    }

    /// Changes a draft order's given header fields, and replaces its lines
    /// when `lines` is given. An order that is no longer a draft is refused
    /// as an invalid transition. Capability [`MANAGE_ORDERS`].
    pub async fn update_order(
        &self,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
        changes: &OrderChanges,
    ) -> Result<Outcome<Order>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_ORDERS,
            "supply_chain.purchasing.order.update",
            |tx| order_lifecycle::update(&mut tx, scope, company_id, order_id, changes)
        )
    }

    /// Submits a draft order for approval; it needs at least one line and an
    /// active supplier. Capability [`MANAGE_ORDERS`].
    pub async fn submit_order(
        &self,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
    ) -> Result<Outcome<Order>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_ORDERS,
            "supply_chain.purchasing.order.submit",
            |tx| order_lifecycle::submit(&mut tx, scope, company_id, order_id)
        )
    }

    /// Approves a submitted order, with an optional `reason` (blank for
    /// none). The approver is not the person who submitted it. From here on
    /// the order changes only through [`Purchasing::amend_order`]. Capability
    /// [`APPROVE_ORDERS`].
    pub async fn approve_order(
        &self,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
        reason: &str,
    ) -> Result<Outcome<Order>> {
        write!(
            self,
            scope,
            company_id,
            APPROVE_ORDERS,
            "supply_chain.purchasing.order.approve",
            |tx| order_lifecycle::approve(&mut tx, scope, company_id, order_id, reason)
        )
    }

    /// Returns a submitted order to draft so it can be corrected and
    /// resubmitted; a `reason` is required. An amended order keeps its trail,
    /// so it is approved or cancelled instead. Capability [`APPROVE_ORDERS`].
    pub async fn return_order(
        &self,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
        reason: &str,
    ) -> Result<Outcome<Order>> {
        write!(
            self,
            scope,
            company_id,
            APPROVE_ORDERS,
            "supply_chain.purchasing.order.return",
            |tx| order_lifecycle::return_to_draft(&mut tx, scope, company_id, order_id, reason)
        )
    }

    /// Marks an approved order as sent to its supplier. Capability
    /// [`MANAGE_ORDERS`].
    pub async fn send_order(
        &self,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
    ) -> Result<Outcome<Order>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_ORDERS,
            "supply_chain.purchasing.order.send",
            |tx| order_lifecycle::send(&mut tx, scope, company_id, order_id)
        )
    }

    /// Closes a sent or partially received order, for example short of its
    /// ordered quantities; a `reason` is required. An order whose lines are
    /// all received closes by itself in [`Purchasing::record_receipt`].
    /// Capability [`MANAGE_ORDERS`].
    pub async fn close_order(
        &self,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
        reason: &str,
    ) -> Result<Outcome<Order>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_ORDERS,
            "supply_chain.purchasing.order.close",
            |tx| order_lifecycle::close(&mut tx, scope, company_id, order_id, reason)
        )
    }

    /// Cancels an order that is not closed or cancelled; a `reason` is
    /// required. An order that was ever approved also needs
    /// [`APPROVE_ORDERS`] (`Error::Forbidden` without it), and one with goods
    /// received against a current line is refused as `GoodsReceived`.
    /// Capability [`MANAGE_ORDERS`].
    pub async fn cancel_order(
        &self,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
        reason: &str,
    ) -> Result<Outcome<Order>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_ORDERS,
            "supply_chain.purchasing.order.cancel",
            |tx| order_lifecycle::cancel(&mut tx, scope, company_id, order_id, reason)
        )
    }

    /// Amends an approved or sent order: records the amendment with its
    /// `reason`, replaces the current lines (at least one; the superseded
    /// ones stay, marked with the amendment) and applies any `reference`,
    /// `notes` or `expected_on` given. A change to the lines returns the
    /// order to submitted for a fresh approval by someone else; a change to
    /// the header facts alone keeps its status. Capability [`MANAGE_ORDERS`].
    pub async fn amend_order(
        &self,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
        amendment: &OrderAmendment,
    ) -> Result<Outcome<Order>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_ORDERS,
            "supply_chain.purchasing.order.amend",
            |tx| order_changes::amend(&mut tx, scope, company_id, order_id, amendment, None)
        )
    }

    /// Agent commits participate in the plan's transaction, never a second one.
    /// The count fences multiple amendments to the same order in one plan.
    pub(crate) async fn amend_order_in(
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
        amendment: &OrderAmendment,
        expected_amendment_count: i32,
    ) -> Result<Outcome<Order>> {
        authorize(tx.sql(), scope, company_id, MANAGE_ORDERS).await?;
        match organisation::require_writable(tx.sql(), scope, company_id, true).await {
            Ok(_) => (),
            Err(WriteError::Archived) => return Ok(Outcome::CompanyArchived),
            Err(WriteError::NotFound) => return Err(Error::NotFound),
            Err(WriteError::Access(error)) => return Err(error),
            Err(_) => return Err(Error::Internal),
        }
        order_changes::amend(
            tx,
            scope,
            company_id,
            order_id,
            amendment,
            Some(expected_amendment_count),
        )
        .await
    }

    /// Records goods received against a sent or partially received order:
    /// each line adds to a current line's received quantity, which stays
    /// within the ordered quantity raised by `tolerance_percent` (0 to 100,
    /// the over-receipt tolerance the caller's company configured). A sent
    /// order becomes partially received; one whose current lines are all
    /// received in full closes. Receiving's seam. Capability
    /// [`RECEIVE_ORDERS`].
    pub async fn record_receipt(
        &self,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
        lines: &[ReceiptLine],
        tolerance_percent: Decimal,
    ) -> Result<Outcome<Order>> {
        write!(
            self,
            scope,
            company_id,
            RECEIVE_ORDERS,
            "supply_chain.purchasing.order.record_receipt",
            |tx| self.record_receipt_in(
                &mut tx,
                scope,
                company_id,
                order_id,
                lines,
                tolerance_percent
            )
        )
    }

    /// Takes back goods recorded as received against a partially received
    /// or closed order when a receipt is reversed: each line subtracts from
    /// a current line's received quantity, never below zero, and a `reason`
    /// is required. A closed order goes back to partially received.
    /// Receiving's seam. Capability [`RECEIVE_ORDERS`].
    pub async fn reverse_receipt(
        &self,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
        lines: &[ReceiptLine],
        reason: &str,
    ) -> Result<Outcome<Order>> {
        write!(
            self,
            scope,
            company_id,
            RECEIVE_ORDERS,
            "supply_chain.purchasing.order.reverse_receipt",
            |tx| self.reverse_receipt_in(&mut tx, scope, company_id, order_id, lines, reason)
        )
    }

    /// Participates in the caller's mutation transaction; never commits it.
    /// The caller must roll back every refusal/error, including its own
    /// earlier writes. Attribution must match the sealed scope. Receiving's
    /// receipt, order changes, audit and outbox then commit together.
    pub async fn record_receipt_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
        lines: &[ReceiptLine],
        tolerance_percent: Decimal,
    ) -> Result<Outcome<Order>> {
        if !receipt_transaction(tx, scope, company_id).await? {
            return Ok(Outcome::CompanyArchived);
        }
        order_changes::record_receipt(tx, scope, company_id, order_id, lines, tolerance_percent)
            .await
    }

    /// The reversal counterpart of [`Self::record_receipt_in`], with the
    /// same authorization, attribution and rollback contract.
    pub async fn reverse_receipt_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
        lines: &[ReceiptLine],
        reason: &str,
    ) -> Result<Outcome<Order>> {
        if !receipt_transaction(tx, scope, company_id).await? {
            return Ok(Outcome::CompanyArchived);
        }
        order_changes::reverse_receipt(tx, scope, company_id, order_id, lines, reason).await
    }

    /// Bilimbi `write/4`: the capability for the target company, then the
    /// company row locked for writing inside the mutation transaction. `None`
    /// is an archived company, already rolled back.
    async fn begin<'c>(
        &self,
        client: &'c mut Client,
        scope: &Scope,
        company_id: i64,
        capability: &str,
        action: &str,
    ) -> Result<Option<MutationTransaction<'c>>> {
        authorize(&*client, scope, company_id, capability).await?;
        let tx = self
            .mutations
            .begin(client, scope.mutation_context(action))
            .await?;
        match organisation::require_writable(tx.sql(), scope, company_id, true).await {
            Ok(_) => Ok(Some(tx)),
            Err(WriteError::Archived) => {
                tx.rollback().await?;
                Ok(None)
            }
            Err(WriteError::NotFound) => Err(Error::NotFound),
            Err(WriteError::Access(error)) => Err(error),
            Err(WriteError::InUse | WriteError::Stale | WriteError::Invalid(_)) => {
                Err(Error::Internal)
            }
        }
    }
}

/// Bilimbi `authorize/3` through Core Company's `authorize_company_target`.
async fn authorize(
    client: &impl GenericClient,
    scope: &Scope,
    company_id: i64,
    capability: &str,
) -> Result<()> {
    organisation::authorize_target(client, scope, company_id, capability)
        .await
        .map(|_| ())
}

/// [`Purchasing::get_order_line`] on any client, so the port answers on the
/// caller's connection or transaction.
async fn order_line(
    client: &impl GenericClient,
    scope: &Scope,
    company_id: i64,
    line_id: i64,
) -> Result<Option<OrderLineFacts>> {
    authorize(client, scope, company_id, VIEW).await?;
    orders::line_facts(client, scope.tenant(), company_id, line_id).await
}

async fn finish<T>(tx: MutationTransaction<'_>, outcome: &Outcome<T>) -> Result<()> {
    match outcome {
        Outcome::Saved(_) => tx.commit().await,
        Outcome::Refused(_) | Outcome::CompanyArchived => tx.rollback().await,
    }
}

/// A sibling's transaction must attribute the same tenant, user and
/// impersonator as the scope used for the business rows and authorization.
async fn receipt_transaction(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
) -> Result<bool> {
    let expected = scope.mutation_context("receipt");
    let context = tx.context();
    let principal = |p: &bilibi_database::Principal| (p.kind.clone(), p.id.clone());
    if context.tenant != expected.tenant
        || principal(&context.actor) != principal(&expected.actor)
        || context.impersonator.as_ref().map(principal)
            != expected.impersonator.as_ref().map(principal)
    {
        return Err(Error::Forbidden);
    }
    authorize(tx.sql(), scope, company_id, RECEIVE_ORDERS).await?;
    match organisation::require_writable(tx.sql(), scope, company_id, true).await {
        Ok(_) => Ok(true),
        Err(WriteError::Archived) => Ok(false),
        Err(WriteError::NotFound) => Err(Error::NotFound),
        Err(WriteError::Access(error)) => Err(error),
        Err(_) => Err(Error::Internal),
    }
}
