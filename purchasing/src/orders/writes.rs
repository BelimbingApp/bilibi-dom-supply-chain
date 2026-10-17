//! The pieces every order write shares: Bilimbi's `OrderLine` changeset
//! rules, the header and line SQL, the history row and a status transition.
//! A child of `orders`, so it reads that module's columns and row mapping.
use super::{
    actor, event_from_row, from_row, line_at, line_change, line_json, saved, to_json, COLUMNS,
    EVENT_COLUMNS, LINE_COLUMNS,
};
use crate::validate::{optional_text, quantity, required_text, Checks, INVALID};
use crate::{
    suppliers, tax_codes, FieldError, NewOrderLine, Order, OrderLine, OrderStatus, Outcome, Refusal,
};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use rust_decimal::Decimal;
use serde_json::json;
use std::collections::HashMap;
use tokio_postgres::types::ToSql;

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
    client: &dyn bilibi_agent_api::OperationClient,
    tenant: i64,
    company_id: i64,
    checks: &mut Checks,
    lines: &[NewOrderLine],
) -> Result<Vec<ValidLine>> {
    let rates: HashMap<i64, Decimal> = tax_codes::list(client, tenant, company_id, true)
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
