//! Purchase orders: the tables' SQL, the reads, and the pieces every order
//! write shares (Bilimbi's `OrderLine` changeset rules, the history row, a
//! status transition). The writes themselves are in `order_lifecycle` and
//! `order_changes`. The database numbers the orders and amendments without
//! gaps, freezes an order once it leaves draft except through a transition,
//! an amendment or a receipt, and refuses a commit without the history or
//! amendment row (migration 002). Private to the facade.
use crate::validate::{optional_text, quantity, required_text, Checks, INVALID};
use crate::{
    suppliers, tax_codes, Amendment, FieldError, NewOrderLine, Order, OrderEvent, OrderLine,
    OrderLineFacts, OrderQuery, OrderStatus, Outcome, Refusal, Supplier,
};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio_postgres::{types::ToSql, GenericClient, Row};

const COLUMNS: &str = "id,order_number,status,purchase_request_id,supplier_id,currency_code,reference,notes,to_char(expected_on,'YYYY-MM-DD'),amendment_count,created_by_user_id,created_by_impersonator_id,submitted_by_user_id,submitted_at,approved_by_user_id,approved_at,sent_at,closed_at,cancelled_at,inserted_at,updated_at";
const LINE_COLUMNS: &str = "id,line_number,description,external_item_ref,quantity,unit,unit_price,tax_code_id,tax_rate,received_quantity,amendment_id,superseded_by_amendment_id";
const EVENT_COLUMNS: &str =
    "id,from_status,to_status,actor_user_id,actor_impersonator_id,reason,occurred_at";
const AMENDMENT_COLUMNS: &str =
    "id,amendment_number,reason,amended_by_user_id,amended_by_impersonator_id,occurred_at";

const HUNDRED: Decimal = Decimal::ONE_HUNDRED;

fn status(value: &str) -> Result<OrderStatus> {
    OrderStatus::parse(value).ok_or(Error::Internal)
}

fn from_row(row: &Row) -> Result<Order> {
    Ok(Order {
        id: row.try_get(0)?,
        order_number: row.try_get(1)?,
        status: status(row.try_get(2)?)?,
        purchase_request_id: row.try_get(3)?,
        purchase_request_number: None,
        supplier_id: row.try_get(4)?,
        supplier: None,
        currency_code: row.try_get(5)?,
        reference: row.try_get(6)?,
        notes: row.try_get(7)?,
        expected_on: row.try_get(8)?,
        amendment_count: row.try_get(9)?,
        created_by_user_id: row.try_get(10)?,
        created_by_impersonator_id: row.try_get(11)?,
        submitted_by_user_id: row.try_get(12)?,
        submitted_at: row.try_get(13)?,
        approved_by_user_id: row.try_get(14)?,
        approved_at: row.try_get(15)?,
        sent_at: row.try_get(16)?,
        closed_at: row.try_get(17)?,
        cancelled_at: row.try_get(18)?,
        inserted_at: row.try_get(19)?,
        updated_at: row.try_get(20)?,
        lines: Vec::new(),
        subtotal: Decimal::ZERO,
        tax_total: Decimal::ZERO,
        total: Decimal::ZERO,
        events: Vec::new(),
        amendments: Vec::new(),
    })
}

/// A line read from `offset` on, with its total and tax worked out as
/// Bilimbi's `line_view/1` does. The facade bounds a line's total when it is
/// written, so the arithmetic only saturates on rows no facade wrote.
fn line_at(row: &Row, offset: usize) -> Result<OrderLine> {
    let quantity: Decimal = row.try_get(offset + 4)?;
    let unit_price: Decimal = row.try_get(offset + 6)?;
    let tax_rate: Option<Decimal> = row.try_get(offset + 8)?;
    let line_total = quantity.saturating_mul(unit_price);
    let tax_amount = tax_rate.map_or(Decimal::ZERO, |rate| {
        line_total
            .saturating_mul(rate)
            .checked_div(HUNDRED)
            .unwrap_or(Decimal::MAX)
    });
    Ok(OrderLine {
        id: row.try_get(offset)?,
        line_number: row.try_get(offset + 1)?,
        description: row.try_get(offset + 2)?,
        external_item_ref: row.try_get(offset + 3)?,
        quantity,
        unit: row.try_get(offset + 5)?,
        unit_price,
        tax_code_id: row.try_get(offset + 7)?,
        tax_rate,
        received_quantity: row.try_get(offset + 9)?,
        amendment_id: row.try_get(offset + 10)?,
        superseded_by_amendment_id: row.try_get(offset + 11)?,
        line_total,
        tax_amount,
    })
}

fn event_from_row(row: &Row) -> Result<OrderEvent> {
    Ok(OrderEvent {
        id: row.try_get(0)?,
        from_status: row.try_get::<_, Option<&str>>(1)?.map(status).transpose()?,
        to_status: status(row.try_get(2)?)?,
        actor_user_id: row.try_get(3)?,
        actor_impersonator_id: row.try_get(4)?,
        reason: row.try_get(5)?,
        occurred_at: row.try_get(6)?,
    })
}

/// The audit view of an order header: its facts and lifecycle actors, not
/// its bookkeeping stamps.
pub(crate) fn to_json(order: &Order) -> Value {
    json!({
        "id": order.id, "order_number": order.order_number, "status": order.status.as_str(),
        "purchase_request_id": order.purchase_request_id, "supplier_id": order.supplier_id,
        "currency_code": order.currency_code, "reference": order.reference,
        "notes": order.notes, "expected_on": order.expected_on,
        "amendment_count": order.amendment_count,
        "created_by_user_id": order.created_by_user_id,
        "created_by_impersonator_id": order.created_by_impersonator_id,
        "submitted_by_user_id": order.submitted_by_user_id,
        "approved_by_user_id": order.approved_by_user_id,
    })
}

pub(crate) fn line_json(order_id: i64, line: &OrderLine) -> Value {
    json!({
        "id": line.id, "order_id": order_id, "line_number": line.line_number,
        "description": line.description, "external_item_ref": line.external_item_ref,
        "quantity": line.quantity.to_string(), "unit": line.unit,
        "unit_price": line.unit_price.to_string(), "tax_code_id": line.tax_code_id,
        "tax_rate": line.tax_rate.map(|rate| rate.to_string()),
        "received_quantity": line.received_quantity.to_string(),
        "amendment_id": line.amendment_id,
        "superseded_by_amendment_id": line.superseded_by_amendment_id,
    })
}

pub(crate) fn line_change(order_id: i64, old: Option<&OrderLine>, new: &OrderLine) -> Change {
    Change {
        kind: "supply_chain.purchasing.order_line".into(),
        id: new.id,
        old: old.map_or_else(|| json!({}), |line| line_json(order_id, line)),
        new: line_json(order_id, new),
    }
}

/// Sets the order's totals from its current lines.
fn total(order: &mut Order) {
    let (subtotal, tax_total) =
        order
            .lines
            .iter()
            .fold((Decimal::ZERO, Decimal::ZERO), |(sub, tax), line| {
                (
                    sub.saturating_add(line.line_total),
                    tax.saturating_add(line.tax_amount),
                )
            });
    order.subtotal = subtotal;
    order.tax_total = tax_total;
    order.total = subtotal.saturating_add(tax_total);
}

/// The current lines (not superseded) of each order in `ids`, by line number.
async fn current_lines(
    client: &impl GenericClient,
    ids: &[i64],
) -> Result<HashMap<i64, Vec<OrderLine>>> {
    let rows = client
        .query(
            &format!(
                "SELECT order_id,{LINE_COLUMNS} FROM supply_chain_purchasing_order_lines \
                 WHERE order_id = ANY($1) AND superseded_by_amendment_id IS NULL ORDER BY order_id,line_number"
            ),
            &[&ids],
        )
        .await?;
    let mut lines: HashMap<i64, Vec<OrderLine>> = HashMap::new();
    for row in &rows {
        lines
            .entry(row.try_get(0)?)
            .or_default()
            .push(line_at(row, 1)?);
    }
    Ok(lines)
}

/// Fills each order's current lines, totals, supplier and request number,
/// as Bilimbi's `order_view/5` does for a list or one order.
async fn complete(
    client: &impl GenericClient,
    tenant: i64,
    company_id: i64,
    orders: &mut [Order],
) -> Result<()> {
    let ids: Vec<i64> = orders.iter().map(|order| order.id).collect();
    let mut lines = current_lines(client, &ids).await?;
    let supplier_ids: Vec<i64> = orders.iter().map(|order| order.supplier_id).collect();
    let suppliers: HashMap<i64, Supplier> =
        suppliers::by_ids(client, tenant, company_id, &supplier_ids)
            .await?
            .into_iter()
            .map(|supplier| (supplier.id, supplier))
            .collect();
    let request_ids: Vec<i64> = orders
        .iter()
        .filter_map(|order| order.purchase_request_id)
        .collect();
    let numbers: HashMap<i64, i64> = client
        .query(
            "SELECT id,request_number FROM supply_chain_purchasing_requests WHERE tenant_id=$1 AND company_id=$2 AND id = ANY($3)",
            &[&tenant, &company_id, &request_ids],
        )
        .await?
        .iter()
        .map(|row| Ok((row.try_get(0)?, row.try_get(1)?)))
        .collect::<Result<_>>()?;
    for order in orders {
        order.lines = lines.remove(&order.id).unwrap_or_default();
        order.supplier = suppliers.get(&order.supplier_id).cloned();
        order.purchase_request_number = order
            .purchase_request_id
            .and_then(|id| numbers.get(&id).copied());
        total(order);
    }
    Ok(())
}

pub(crate) async fn list(
    client: &impl GenericClient,
    tenant: i64,
    company_id: i64,
    query: OrderQuery,
) -> Result<Vec<Order>> {
    let status = query.status.map(OrderStatus::as_str);
    let mut orders = client
        .query(
            &format!(
                "SELECT {COLUMNS} FROM supply_chain_purchasing_orders \
                 WHERE tenant_id=$1 AND company_id=$2 AND ($3::text IS NULL OR status=$3) \
                 AND ($4::bigint IS NULL OR supplier_id=$4) ORDER BY order_number DESC LIMIT $5"
            ),
            &[
                &tenant,
                &company_id,
                &status,
                &query.supplier_id,
                &query.limit,
            ],
        )
        .await?
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>>>()?;
    complete(client, tenant, company_id, &mut orders).await?;
    Ok(orders)
}

/// The header only, `FOR UPDATE` when `lock`: a write reads its order
/// locked, so a transition is judged on the status it will change.
async fn header(
    client: &impl GenericClient,
    tenant: i64,
    company_id: i64,
    filter: &str,
    key: i64,
    lock: bool,
) -> Result<Option<Order>> {
    client
        .query_opt(
            &format!(
                "SELECT {COLUMNS} FROM supply_chain_purchasing_orders WHERE tenant_id=$1 AND company_id=$2 AND {filter}=$3{}",
                if lock { " FOR UPDATE" } else { "" }
            ),
            &[&tenant, &company_id, &key],
        )
        .await?
        .as_ref()
        .map(from_row)
        .transpose()
}

/// The order's status history and its amendments, oldest first, each
/// amendment with the lines it added and the IDs of those it superseded.
async fn history(client: &impl GenericClient, order: &mut Order) -> Result<()> {
    order.events = client
        .query(
            &format!("SELECT {EVENT_COLUMNS} FROM supply_chain_purchasing_order_events WHERE order_id=$1 ORDER BY id"),
            &[&order.id],
        )
        .await?
        .iter()
        .map(event_from_row)
        .collect::<Result<_>>()?;
    let amendments = client
        .query(
            &format!("SELECT {AMENDMENT_COLUMNS} FROM supply_chain_purchasing_order_amendments WHERE order_id=$1 ORDER BY amendment_number"),
            &[&order.id],
        )
        .await?;
    if amendments.is_empty() {
        order.amendments = Vec::new();
        return Ok(());
    }
    let lines = client
        .query(
            &format!(
                "SELECT {LINE_COLUMNS} FROM supply_chain_purchasing_order_lines WHERE order_id=$1 \
                 AND (amendment_id IS NOT NULL OR superseded_by_amendment_id IS NOT NULL) ORDER BY id"
            ),
            &[&order.id],
        )
        .await?
        .iter()
        .map(|row| line_at(row, 0))
        .collect::<Result<Vec<_>>>()?;
    order.amendments = amendments
        .iter()
        .map(|row| {
            let id: i64 = row.try_get(0)?;
            Ok(Amendment {
                id,
                amendment_number: row.try_get(1)?,
                reason: row.try_get(2)?,
                amended_by_user_id: row.try_get(3)?,
                amended_by_impersonator_id: row.try_get(4)?,
                occurred_at: row.try_get(5)?,
                lines: lines
                    .iter()
                    .filter(|line| line.amendment_id == Some(id))
                    .cloned()
                    .collect(),
                superseded_line_ids: lines
                    .iter()
                    .filter(|line| line.superseded_by_amendment_id == Some(id))
                    .map(|line| line.id)
                    .collect(),
            })
        })
        .collect::<Result<_>>()?;
    Ok(())
}

/// One order as `get_order` returns it: header, supplier, current lines with
/// totals, history and amendments.
async fn full(
    client: &impl GenericClient,
    tenant: i64,
    company_id: i64,
    filter: &str,
    key: i64,
) -> Result<Option<Order>> {
    let Some(order) = header(client, tenant, company_id, filter, key, false).await? else {
        return Ok(None);
    };
    let mut orders = [order];
    complete(client, tenant, company_id, &mut orders).await?;
    let [mut order] = orders;
    history(client, &mut order).await?;
    Ok(Some(order))
}

pub(crate) async fn get(
    client: &impl GenericClient,
    tenant: i64,
    company_id: i64,
    order_id: i64,
) -> Result<Option<Order>> {
    full(client, tenant, company_id, "id", order_id).await
}

pub(crate) async fn find_by_number(
    client: &impl GenericClient,
    tenant: i64,
    company_id: i64,
    order_number: i64,
) -> Result<Option<Order>> {
    full(client, tenant, company_id, "order_number", order_number).await
}

/// One current or superseded line of an order of the company, with its
/// order's facts.
pub(crate) async fn line_facts(
    client: &impl GenericClient,
    tenant: i64,
    company_id: i64,
    line_id: i64,
) -> Result<Option<OrderLineFacts>> {
    let columns = LINE_COLUMNS
        .split(',')
        .map(|column| format!("l.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    let Some(row) = client
        .query_opt(
            &format!(
                "SELECT {columns},o.id,o.order_number,o.status,o.supplier_id,o.currency_code \
                 FROM supply_chain_purchasing_order_lines l JOIN supply_chain_purchasing_orders o ON o.id=l.order_id \
                 WHERE l.tenant_id=$1 AND o.tenant_id=$1 AND o.company_id=$2 AND l.id=$3"
            ),
            &[&tenant, &company_id, &line_id],
        )
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(OrderLineFacts {
        line: line_at(&row, 0)?,
        order_id: row.try_get(12)?,
        order_number: row.try_get(13)?,
        order_status: status(row.try_get(14)?)?,
        supplier_id: row.try_get(15)?,
        currency_code: row.try_get(16)?,
    }))
}

/// The locked order with its current lines, or the refusal a missing one is.
pub(crate) async fn locked(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    order_id: i64,
) -> Result<std::result::Result<Order, Refusal>> {
    let Some(mut order) =
        header(tx.sql(), scope.tenant(), company_id, "id", order_id, true).await?
    else {
        return Ok(Err(Refusal::OrderNotFound));
    };
    order.lines = current_lines(tx.sql(), &[order.id])
        .await?
        .remove(&order.id)
        .unwrap_or_default();
    Ok(Ok(order))
}

/// The saved order as `get` reads it inside the write's transaction.
pub(crate) async fn saved(
    tx: &MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    order_id: i64,
) -> Result<Outcome<Order>> {
    get(tx.sql(), tenant, company_id, order_id)
        .await?
        .map(Outcome::Saved)
        .ok_or(Error::Internal)
}

pub(crate) fn require_status(
    order: &Order,
    allowed: &[OrderStatus],
    target: &'static str,
) -> std::result::Result<(), Refusal> {
    if allowed.contains(&order.status) {
        Ok(())
    } else {
        Err(Refusal::InvalidTransition {
            status: order.status.as_str(),
            target,
        })
    }
}

/// Who acts, from the sealed scope: the signed-in account and any
/// impersonating operator.
pub(crate) fn actor(scope: &Scope) -> (i64, Option<i64>) {
    (
        scope.user(),
        scope.impersonator().map(|operator| operator.user()),
    )
}

/// Bilimbi `validate_supplier/3`: a supplier named on a draft is an active
/// supplier of this company. The company lock serializes this with
/// deactivation.
pub(crate) async fn active_supplier(
    tx: &MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    supplier_id: i64,
) -> Result<std::result::Result<(), Refusal>> {
    Ok(
        match suppliers::get(tx.sql(), tenant, company_id, supplier_id).await? {
            None => Err(Refusal::SupplierNotFound),
            Some(supplier) if !supplier.active => Err(Refusal::SupplierInactive),
            Some(_) => Ok(()),
        },
    )
}

/// A line as `OrderLine.changeset/2` accepts it, with the rate its tax code
/// has now.
#[derive(Clone, Default)]
pub(crate) struct ValidLine {
    pub(crate) description: String,
    pub(crate) external_item_ref: Option<String>,
    pub(crate) quantity: Decimal,
    pub(crate) unit: String,
    pub(crate) unit_price: Decimal,
    pub(crate) tax_code_id: Option<i64>,
    pub(crate) tax_rate: Option<Decimal>,
}

/// numeric(20, 4): sixteen digits before the point.
const AMOUNT_LIMIT: Decimal = Decimal::from_parts(1_874_919_424, 2_328_306, 0, false, 0);
/// The largest line total the facade writes, so an order's totals always fit
/// the decimal type the reads compute them in.
const LINE_TOTAL_LIMIT: Decimal = Decimal::from_parts(1_661_992_960, 1_808_227_885, 5, false, 0);

fn unit_price(value: &str) -> std::result::Result<Decimal, FieldError> {
    let price = crate::validate::decimal("unit_price", value, 4)?
        .ok_or_else(|| FieldError::new("unit_price", crate::validate::BLANK))?;
    if price < Decimal::ZERO {
        return Err(FieldError::new(
            "unit_price",
            "must be greater than or equal to 0",
        ));
    }
    if price >= AMOUNT_LIMIT {
        return Err(FieldError::new("unit_price", "must be less than 1E+16"));
    }
    Ok(price)
}

/// Checks every line against the rules and the company's active tax codes,
/// each line's errors numbered from 1.
pub(crate) async fn validate_lines(
    tx: &MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    checks: &mut Checks,
    lines: &[NewOrderLine],
) -> Result<Vec<ValidLine>> {
    let rates: HashMap<i64, Decimal> = tax_codes::list(tx.sql(), tenant, company_id, true)
        .await?
        .into_iter()
        .map(|tax_code| (tax_code.id, tax_code.rate))
        .collect();
    Ok(lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let number = index + 1;
            let mut valid = ValidLine {
                description: checks
                    .take_line(number, required_text("description", &line.description, 500)),
                external_item_ref: checks.take_line(
                    number,
                    optional_text("external_item_ref", &line.external_item_ref, 100),
                ),
                quantity: checks.take_line(number, quantity("quantity", &line.quantity)),
                unit: checks.take_line(number, required_text("unit", &line.unit, 20)),
                unit_price: checks.take_line(number, unit_price(&line.unit_price)),
                tax_code_id: line.tax_code_id,
                tax_rate: None,
            };
            if let Some(id) = line.tax_code_id {
                valid.tax_rate = checks.take_line(
                    number,
                    rates.get(&id).copied().map(Some).ok_or_else(|| {
                        FieldError::new("tax_code_id", "is not an active tax code of this company")
                    }),
                );
            }
            if valid
                .quantity
                .checked_mul(valid.unit_price)
                .is_none_or(|line_total| line_total >= LINE_TOTAL_LIMIT)
            {
                checks.take_line(
                    number,
                    Err::<(), _>(FieldError::new(
                        "unit_price",
                        "makes the line total too large",
                    )),
                );
            }
            valid
        })
        .collect())
}

/// An optional ISO date or an `is invalid` refusal of `field`.
pub(crate) fn date(
    field: &'static str,
    value: &str,
) -> std::result::Result<Option<String>, FieldError> {
    crate::validate::date(field, value).map_err(|_| FieldError::new(field, INVALID))
}

/// Inserts `valid` as the order's lines 1, 2, 3, ... (added by `amendment`
/// when given) and returns them; their `Change`s go on `changes`.
pub(crate) async fn insert_lines(
    sql: &tokio_postgres::Transaction<'_>,
    tenant: i64,
    company_id: i64,
    order_id: i64,
    amendment: Option<i64>,
    valid: &[ValidLine],
    changes: &mut Vec<Change>,
) -> Result<Vec<OrderLine>> {
    let mut inserted = Vec::with_capacity(valid.len());
    for (index, line) in valid.iter().enumerate() {
        let number = i32::try_from(index + 1).map_err(|_| Error::Invalid)?;
        let row = sql
            .query_one(
                &format!(
                    "INSERT INTO supply_chain_purchasing_order_lines(tenant_id,company_id,order_id,amendment_id,line_number,description,external_item_ref,quantity,unit,unit_price,tax_code_id,tax_rate) \
                     VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING {LINE_COLUMNS}"
                ),
                &[
                    &tenant, &company_id, &order_id, &amendment, &number, &line.description,
                    &line.external_item_ref, &line.quantity, &line.unit, &line.unit_price,
                    &line.tax_code_id, &line.tax_rate,
                ],
            )
            .await?;
        let line = line_at(&row, 0)?;
        changes.push(line_change(order_id, None, &line));
        inserted.push(line);
    }
    Ok(inserted)
}

/// Inserts the order header and returns it; the trigger numbers it and the
/// status defaults to draft.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_header(
    sql: &tokio_postgres::Transaction<'_>,
    tenant: i64,
    company_id: i64,
    request_id: Option<i64>,
    supplier_id: i64,
    currency_code: &str,
    amendable: &Amendable,
    actor: (i64, Option<i64>),
) -> Result<Order> {
    let row = sql
        .query_one(
            &format!(
                "INSERT INTO supply_chain_purchasing_orders(tenant_id,company_id,purchase_request_id,supplier_id,currency_code,reference,notes,expected_on,created_by_user_id,created_by_impersonator_id,inserted_at,updated_at) \
                 VALUES($1,$2,$3,$4,$5,$6,$7,$8::text::date,$9,$10,now() AT TIME ZONE 'UTC',now() AT TIME ZONE 'UTC') RETURNING {COLUMNS}"
            ),
            &[
                &tenant, &company_id, &request_id, &supplier_id, &currency_code,
                &amendable.reference, &amendable.notes, &amendable.expected_on, &actor.0, &actor.1,
            ],
        )
        .await?;
    from_row(&row)
}

/// The facts an amendment may change (Bilimbi `amendable_changeset/2`).
#[derive(Clone, Default)]
pub(crate) struct Amendable {
    pub(crate) reference: Option<String>,
    pub(crate) notes: Option<String>,
    pub(crate) expected_on: Option<String>,
}
impl Amendable {
    pub(crate) fn of(order: &Order) -> Self {
        Self {
            reference: order.reference.clone(),
            notes: order.notes.clone(),
            expected_on: order.expected_on.clone(),
        }
    }
    /// Applies the given values, checking each; `None` keeps the current one.
    pub(crate) fn apply(
        &mut self,
        checks: &mut Checks,
        reference: Option<&str>,
        notes: Option<&str>,
        expected_on: Option<&str>,
    ) {
        if let Some(value) = reference {
            self.reference = checks.take(optional_text("reference", value, 100));
        }
        if let Some(value) = notes {
            self.notes = checks.take(optional_text("notes", value, 1000));
        }
        if let Some(value) = expected_on {
            self.expected_on = checks.take(date("expected_on", value));
        }
    }
}

/// A currency code: trimmed, upper-cased, three letters.
pub(crate) fn currency_code(value: &str) -> std::result::Result<String, FieldError> {
    let value = value.trim().to_uppercase();
    if value.is_empty() {
        return Err(FieldError::new("currency_code", crate::validate::BLANK));
    }
    if value.len() == 3 && value.bytes().all(|c| c.is_ascii_uppercase()) {
        Ok(value)
    } else {
        Err(FieldError::new(
            "currency_code",
            crate::validate::INVALID_FORMAT,
        ))
    }
}

/// Appends the history row for `order` having become its current status
/// from `from`; the database stamps its time with `clock_timestamp()`.
pub(crate) async fn insert_event(
    sql: &tokio_postgres::Transaction<'_>,
    tenant: i64,
    company_id: i64,
    order: &Order,
    from: Option<OrderStatus>,
    actor: (i64, Option<i64>),
    reason: Option<&str>,
) -> Result<Change> {
    let row = sql
        .query_one(
            &format!(
                "INSERT INTO supply_chain_purchasing_order_events(tenant_id,company_id,order_id,from_status,to_status,actor_user_id,actor_impersonator_id,reason) \
                 VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING {EVENT_COLUMNS}"
            ),
            &[
                &tenant, &company_id, &order.id, &from.map(OrderStatus::as_str),
                &order.status.as_str(), &actor.0, &actor.1, &reason,
            ],
        )
        .await?;
    let event = event_from_row(&row)?;
    Ok(Change {
        kind: "supply_chain.purchasing.order_event".into(),
        id: event.id,
        old: json!({}),
        new: json!({
            "id": event.id, "order_id": order.id,
            "from_status": event.from_status.map(OrderStatus::as_str),
            "to_status": event.to_status.as_str(), "actor_user_id": event.actor_user_id,
            "actor_impersonator_id": event.actor_impersonator_id, "reason": event.reason,
        }),
    })
}

/// The lifecycle columns a transition sets, beside the status. `$4` is the
/// acting user.
#[derive(Clone, Copy)]
pub(crate) enum Stamps {
    None,
    Submitted,
    /// Back to submitted by an amendment: the approval and sending go.
    Resubmitted,
    Approved,
    /// Back to draft: the submission goes.
    Returned,
    Sent,
    Closed,
    /// Closed back to partially received: the closing goes.
    Reopened,
    Cancelled,
}
impl Stamps {
    fn sql(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Submitted => ",submitted_by_user_id=$4,submitted_at=clock_timestamp() AT TIME ZONE 'UTC'",
            Self::Resubmitted => ",submitted_by_user_id=$4,submitted_at=clock_timestamp() AT TIME ZONE 'UTC',approved_by_user_id=NULL,approved_at=NULL,sent_at=NULL",
            Self::Approved => ",approved_by_user_id=$4,approved_at=clock_timestamp() AT TIME ZONE 'UTC'",
            Self::Returned => ",submitted_by_user_id=NULL,submitted_at=NULL",
            Self::Sent => ",sent_at=clock_timestamp() AT TIME ZONE 'UTC'",
            Self::Closed => ",closed_at=clock_timestamp() AT TIME ZONE 'UTC'",
            Self::Reopened => ",closed_at=NULL",
            Self::Cancelled => ",cancelled_at=clock_timestamp() AT TIME ZONE 'UTC'",
        }
    }
}

/// One status transition and its history row, inside a `mutate` closure.
/// Returns the updated header (its lines are those of `current`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn transition_in(
    sql: &tokio_postgres::Transaction<'_>,
    tenant: i64,
    company_id: i64,
    current: &Order,
    to: OrderStatus,
    stamps: Stamps,
    actor: (i64, Option<i64>),
    reason: Option<&str>,
    changes: &mut Vec<Change>,
) -> Result<Order> {
    let stamps = stamps.sql();
    let status = to.as_str();
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![&company_id, &current.id, &status];
    if stamps.contains("$4") {
        params.push(&actor.0);
    }
    let row = sql
        .query_one(
            &format!(
                "UPDATE supply_chain_purchasing_orders SET status=$3{stamps},updated_at=now() AT TIME ZONE 'UTC' \
                 WHERE company_id=$1 AND id=$2 RETURNING {COLUMNS}"
            ),
            &params,
        )
        .await?;
    let mut updated = from_row(&row)?;
    updated.lines = current.lines.clone();
    changes.push(Change {
        kind: "supply_chain.purchasing.order".into(),
        id: updated.id,
        old: to_json(current),
        new: to_json(&updated),
    });
    changes.push(
        insert_event(
            sql,
            tenant,
            company_id,
            &updated,
            Some(current.status),
            actor,
            reason,
        )
        .await?,
    );
    Ok(updated)
}

/// A whole transition as one mutation, answering the saved order.
pub(crate) async fn transition(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    current: Order,
    to: OrderStatus,
    stamps: Stamps,
    reason: Option<String>,
) -> Result<Outcome<Order>> {
    let tenant = scope.tenant();
    let actor = actor(scope);
    let order_id = current.id;
    tx.mutate(move |sql| {
        Box::pin(async move {
            let mut changes = Vec::new();
            transition_in(
                sql,
                tenant,
                company_id,
                &current,
                to,
                stamps,
                actor,
                reason.as_deref(),
                &mut changes,
            )
            .await?;
            Ok(((), changes))
        })
    })
    .await?;
    saved(tx, tenant, company_id, order_id).await
}

/// Updates one line's received quantity, inside a `mutate` closure.
pub(crate) async fn set_received(
    sql: &tokio_postgres::Transaction<'_>,
    order_id: i64,
    line: &OrderLine,
    received: Decimal,
    changes: &mut Vec<Change>,
) -> Result<OrderLine> {
    let row = sql
        .query_one(
            &format!(
                "UPDATE supply_chain_purchasing_order_lines SET received_quantity=$3 WHERE order_id=$1 AND id=$2 RETURNING {LINE_COLUMNS}"
            ),
            &[&order_id, &line.id, &received],
        )
        .await?;
    let updated = line_at(&row, 0)?;
    changes.push(line_change(order_id, Some(line), &updated));
    Ok(updated)
}

/// Supersedes every current line of the order by `amendment`, inside a
/// `mutate` closure.
pub(crate) async fn supersede_lines(
    sql: &tokio_postgres::Transaction<'_>,
    order: &Order,
    amendment: i64,
    changes: &mut Vec<Change>,
) -> Result<()> {
    let rows = sql
        .query(
            &format!(
                "UPDATE supply_chain_purchasing_order_lines SET superseded_by_amendment_id=$2 \
                 WHERE order_id=$1 AND superseded_by_amendment_id IS NULL RETURNING {LINE_COLUMNS}"
            ),
            &[&order.id, &amendment],
        )
        .await?;
    for row in &rows {
        let updated = line_at(row, 0)?;
        let old = order.lines.iter().find(|line| line.id == updated.id);
        changes.push(line_change(order.id, old, &updated));
    }
    Ok(())
}

/// Deletes a draft's lines, as Bilimbi's `replace_lines/4` does before
/// inserting the new ones, inside a `mutate` closure.
pub(crate) async fn delete_draft_lines(
    sql: &tokio_postgres::Transaction<'_>,
    order: &Order,
    changes: &mut Vec<Change>,
) -> Result<()> {
    sql.execute(
        "DELETE FROM supply_chain_purchasing_order_lines WHERE order_id=$1",
        &[&order.id],
    )
    .await?;
    for line in &order.lines {
        changes.push(Change {
            kind: "supply_chain.purchasing.order_line".into(),
            id: line.id,
            old: line_json(order.id, line),
            new: json!({}),
        });
    }
    Ok(())
}

/// Updates a draft's header, or applies an amendment's facts with its count,
/// inside a `mutate` closure.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn update_header(
    sql: &tokio_postgres::Transaction<'_>,
    company_id: i64,
    current: &Order,
    supplier_id: i64,
    currency_code: &str,
    amendable: &Amendable,
    amendment_count: i32,
    changes: &mut Vec<Change>,
) -> Result<Order> {
    let row = sql
        .query_one(
            &format!(
                "UPDATE supply_chain_purchasing_orders SET supplier_id=$3,currency_code=$4,reference=$5,notes=$6,expected_on=$7::text::date,amendment_count=$8,updated_at=now() AT TIME ZONE 'UTC' \
                 WHERE company_id=$1 AND id=$2 RETURNING {COLUMNS}"
            ),
            &[
                &company_id, &current.id, &supplier_id, &currency_code,
                &amendable.reference, &amendable.notes, &amendable.expected_on, &amendment_count,
            ],
        )
        .await?;
    let mut updated = from_row(&row)?;
    updated.lines = current.lines.clone();
    changes.push(Change {
        kind: "supply_chain.purchasing.order".into(),
        id: updated.id,
        old: to_json(current),
        new: to_json(&updated),
    });
    Ok(updated)
}
