//! Purchase orders: the tables' SQL, the reads, and the pieces every order
//! write shares (Bilimbi's `OrderLine` changeset rules, the history row, a
//! status transition). The writes themselves are in `order_lifecycle` and
//! `order_changes`. The database numbers the orders and amendments without
//! gaps, freezes an order once it leaves draft except through a transition,
//! an amendment or a receipt, and refuses a commit without the history or
//! amendment row (migration 002). Private to the facade.
use crate::{
    suppliers, Amendment, Order, OrderEvent, OrderLine, OrderLineFacts, OrderQuery, OrderStatus,
    Outcome, Refusal, Supplier,
};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio_postgres::{GenericClient, Row};

const COLUMNS: &str = "id,order_number,status,purchase_request_id,supplier_id,currency_code,reference,notes,to_char(expected_on,'YYYY-MM-DD'),amendment_count,created_by_user_id,created_by_impersonator_id,submitted_by_user_id,submitted_at,approved_by_user_id,approved_at,sent_at,closed_at,cancelled_at,inserted_at,updated_at";
const LINE_COLUMNS: &str = "id,line_number,description,external_item_ref,quantity,unit,unit_price,tax_code_id,tax_rate,received_quantity,amendment_id,superseded_by_amendment_id";
const EVENT_COLUMNS: &str =
    "id,from_status,to_status,actor_user_id,actor_impersonator_id,reason,occurred_at";
const AMENDMENT_COLUMNS: &str =
    "id,amendment_number,reason,amended_by_user_id,amended_by_impersonator_id,occurred_at";

const HUNDRED: Decimal = Decimal::ONE_HUNDRED;

mod writes;
pub(crate) use writes::*;

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
