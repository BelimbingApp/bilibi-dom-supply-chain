//! Purchase orders: so far only Bilimbi's `create_order_from_request/4` and
//! the header rules of `Order.create_changeset/2` it runs, so an approved
//! request becomes a draft order. The rest of the order lifecycle (create,
//! update, submit, approve, send, amend, receive, close) follows in its own
//! slice over the same tables (migration 002), which number the orders
//! without gaps and refuse a commit without a history row. Private to the
//! facade.
use crate::validate::{Checks, BLANK, INVALID_FORMAT};
use crate::{
    FieldError, Order, OrderFromRequest, OrderStatus, Outcome, Refusal, Request, RequestStatus,
};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use tokio_postgres::Row;

const COLUMNS: &str = "id,order_number,purchase_request_id,supplier_id,currency_code,status";

fn from_row(row: &Row) -> Result<Order> {
    Ok(Order {
        id: row.try_get(0)?,
        order_number: row.try_get(1)?,
        purchase_request_id: row.try_get(2)?,
        supplier_id: row.try_get(3)?,
        currency_code: row.try_get(4)?,
        status: OrderStatus::parse(row.try_get(5)?).ok_or(Error::Internal)?,
    })
}

/// The audit view of an order header.
fn to_json(order: &Order, actor: (i64, Option<i64>)) -> Value {
    json!({
        "id": order.id, "order_number": order.order_number,
        "purchase_request_id": order.purchase_request_id, "supplier_id": order.supplier_id,
        "currency_code": order.currency_code, "status": order.status.as_str(),
        "created_by_user_id": actor.0, "created_by_impersonator_id": actor.1,
    })
}

fn supplier_id(value: Option<i64>) -> std::result::Result<i64, FieldError> {
    value.ok_or_else(|| FieldError::new("supplier_id", BLANK))
}

/// Trimmed and upper-cased, then three letters (`^[A-Z]{3}$`).
fn currency_code(value: &str) -> std::result::Result<String, FieldError> {
    let value = value.trim().to_uppercase();
    if value.is_empty() {
        return Err(FieldError::new("currency_code", BLANK));
    }
    if value.len() == 3 && value.bytes().all(|c| c.is_ascii_uppercase()) {
        Ok(value)
    } else {
        Err(FieldError::new("currency_code", INVALID_FORMAT))
    }
}

/// A draft order from an approved request: its supplier and currency as
/// given, and a copy of each request line priced at zero with no tax code,
/// for the buyer to price on the draft. The request stays approved and may
/// be ordered again (a split order).
pub(crate) async fn create_from_request(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    request: Request,
    input: &OrderFromRequest,
) -> Result<Outcome<Order>> {
    if request.status != RequestStatus::Approved {
        return Ok(Outcome::Refused(Refusal::InvalidTransition {
            status: request.status.as_str(),
            target: "ordered",
        }));
    }
    let mut checks = Checks::default();
    let supplier_id = checks.take(supplier_id(input.supplier_id));
    let currency_code = checks.take(currency_code(&input.currency_code));
    if let Err(errors) = checks.finish() {
        return Ok(Outcome::Refused(Refusal::Invalid(errors)));
    }
    let tenant = scope.tenant();
    // The company lock the write holds serializes this with deactivation.
    let active = tx
        .sql()
        .query_opt(
            "SELECT active FROM supply_chain_purchasing_suppliers WHERE tenant_id=$1 AND company_id=$2 AND id=$3",
            &[&tenant, &company_id, &supplier_id],
        )
        .await?
        .map(|row| row.try_get::<_, bool>(0))
        .transpose()?;
    match active {
        None => return Ok(Outcome::Refused(Refusal::SupplierNotFound)),
        Some(false) => return Ok(Outcome::Refused(Refusal::SupplierInactive)),
        Some(true) => {}
    }
    let actor = (
        scope.user(),
        scope.impersonator().map(|operator| operator.user()),
    );
    let order = tx
        .mutate(move |sql| {
            Box::pin(async move {
                // The trigger numbers the order; the status defaults to draft.
                let row = sql
                    .query_one(
                        &format!(
                            "INSERT INTO supply_chain_purchasing_orders(tenant_id,company_id,purchase_request_id,supplier_id,currency_code,created_by_user_id,created_by_impersonator_id,inserted_at,updated_at) \
                             VALUES($1,$2,$3,$4,$5,$6,$7,now() AT TIME ZONE 'UTC',now() AT TIME ZONE 'UTC') RETURNING {COLUMNS}"
                        ),
                        &[&tenant, &company_id, &request.id, &supplier_id, &currency_code, &actor.0, &actor.1],
                    )
                    .await?;
                let order = from_row(&row)?;
                let mut changes = vec![Change {
                    kind: "supply_chain.purchasing.order".into(),
                    id: order.id,
                    old: json!({}),
                    new: to_json(&order, actor),
                }];
                for line in &request.lines {
                    let row = sql
                        .query_one(
                            "INSERT INTO supply_chain_purchasing_order_lines(tenant_id,company_id,order_id,line_number,description,external_item_ref,quantity,unit,unit_price) \
                             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id",
                            &[
                                &tenant, &company_id, &order.id, &line.line_number, &line.description,
                                &line.external_item_ref, &line.quantity, &line.unit, &Decimal::ZERO,
                            ],
                        )
                        .await?;
                    let id: i64 = row.try_get(0)?;
                    changes.push(Change {
                        kind: "supply_chain.purchasing.order_line".into(),
                        id,
                        old: json!({}),
                        new: json!({
                            "id": id, "order_id": order.id, "line_number": line.line_number,
                            "description": line.description, "external_item_ref": line.external_item_ref,
                            "quantity": line.quantity.to_string(), "unit": line.unit,
                            "unit_price": Decimal::ZERO.to_string(), "tax_code_id": null, "tax_rate": null,
                        }),
                    });
                }
                let row = sql
                    .query_one(
                        "INSERT INTO supply_chain_purchasing_order_events(tenant_id,company_id,order_id,from_status,to_status,actor_user_id,actor_impersonator_id) \
                         VALUES($1,$2,$3,NULL,$4,$5,$6) RETURNING id",
                        &[&tenant, &company_id, &order.id, &order.status.as_str(), &actor.0, &actor.1],
                    )
                    .await?;
                let event_id: i64 = row.try_get(0)?;
                changes.push(Change {
                    kind: "supply_chain.purchasing.order_event".into(),
                    id: event_id,
                    old: json!({}),
                    new: json!({
                        "id": event_id, "order_id": order.id, "from_status": null,
                        "to_status": order.status.as_str(), "actor_user_id": actor.0,
                        "actor_impersonator_id": actor.1, "reason": null,
                    }),
                });
                Ok((order, changes))
            })
        })
        .await?;
    Ok(Outcome::Saved(order))
}
