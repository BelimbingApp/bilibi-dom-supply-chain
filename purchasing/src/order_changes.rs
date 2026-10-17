//! What changes an order after approval (Bilimbi `amend_order/4`,
//! `record_receipt/4` and `reverse_receipt/4`): an amendment that replaces
//! the current lines and sends a changed order back for approval, and goods
//! recorded as received against a sent order or taken back when a receipt is
//! reversed. Private to the facade.
use crate::order_lifecycle::refuse;
use crate::orders::{
    actor, insert_lines, locked, require_status, saved, set_received, supersede_lines,
    transition_in, update_header, validate_lines, Amendable, Stamps, ValidLine,
};
use crate::validate::{reason, Checks};
use crate::{
    FieldError, Order, OrderAmendment, OrderLine, OrderStatus, Outcome, ReceiptLine, Refusal,
};
use bilibi_database::{Change, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use rust_decimal::Decimal;
use serde_json::json;

/// What an approver agreed to: the lines, their quantities, prices and tax.
fn lines_changed(before: &[OrderLine], after: &[ValidLine]) -> bool {
    before.len() != after.len()
        || before.iter().zip(after).any(|(old, new)| {
            old.description != new.description
                || old.external_item_ref != new.external_item_ref
                || old.unit != new.unit
                || old.tax_code_id != new.tax_code_id
                || old.quantity != new.quantity
                || old.unit_price != new.unit_price
                || old.tax_rate != new.tax_rate
        })
}

/// Shared, nonmutating amendment preparation for the facade and agent review.
pub(crate) struct PreparedAmendment {
    pub(crate) reason: String,
    pub(crate) header: Amendable,
    pub(crate) lines: Vec<ValidLine>,
    pub(crate) resubmitted: bool,
}

pub(crate) async fn prepare_amendment(
    client: &dyn bilibi_agent_api::OperationClient,
    tenant: i64,
    company_id: i64,
    current: &Order,
    input: &OrderAmendment,
) -> Result<std::result::Result<PreparedAmendment, Refusal>> {
    if let Err(refusal) = require_status(
        current,
        &[OrderStatus::Approved, OrderStatus::Sent],
        "amended",
    ) {
        return Ok(Err(refusal));
    }
    if input.lines.is_empty() {
        return Ok(Err(Refusal::NoLines));
    }
    let mut checks = Checks::default();
    let reason = checks.take(reason(&input.reason, true)).unwrap_or_default();
    let mut header = Amendable::of(current);
    header.apply(
        &mut checks,
        input.reference.as_deref(),
        input.notes.as_deref(),
        input.expected_on.as_deref(),
    );
    let lines = validate_lines(client, tenant, company_id, &mut checks, &input.lines).await?;
    Ok(checks
        .finish()
        .map_err(Refusal::Invalid)
        .map(|()| PreparedAmendment {
            reason,
            header,
            resubmitted: lines_changed(&current.lines, &lines),
            lines,
        }))
}

/// An approved or sent order amended: the amendment row, the header's
/// amendable facts and count, the current lines superseded and the new ones
/// added, all in one mutation. A change to the lines also returns the order
/// to submitted, clearing its approval and sending, so someone other than
/// the amender approves it again.
pub(crate) async fn amend(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    order_id: i64,
    input: &OrderAmendment,
    expected_amendment_count: Option<i32>,
) -> Result<Outcome<Order>> {
    let tenant = scope.tenant();
    let current = refuse!(locked(tx, scope, company_id, order_id).await?);
    if expected_amendment_count.is_some_and(|count| count != current.amendment_count) {
        return Err(bilibi_database::Error::Conflict);
    }
    let prepared = refuse!(prepare_amendment(tx.sql(), tenant, company_id, &current, input).await?);
    let PreparedAmendment {
        reason,
        header: amendable,
        lines: valid_lines,
        resubmitted,
    } = prepared;
    let actor = actor(scope);
    tx.mutate(move |sql| {
        Box::pin(async move {
            // The trigger numbers the amendment and refuses one of an order
            // that is not approved or sent.
            let row = sql
                .query_one(
                    "INSERT INTO supply_chain_purchasing_order_amendments(tenant_id,company_id,order_id,reason,amended_by_user_id,amended_by_impersonator_id) \
                     VALUES($1,$2,$3,$4,$5,$6) RETURNING id,amendment_number",
                    &[&tenant, &company_id, &current.id, &reason, &actor.0, &actor.1],
                )
                .await?;
            let amendment_id: i64 = row.try_get(0)?;
            let amendment_number: i32 = row.try_get(1)?;
            let mut changes = vec![Change {
                kind: "supply_chain.purchasing.order_amendment".into(),
                id: amendment_id,
                old: json!({}),
                new: json!({
                    "id": amendment_id, "order_id": current.id,
                    "amendment_number": amendment_number, "reason": reason,
                    "amended_by_user_id": actor.0, "amended_by_impersonator_id": actor.1,
                }),
            }];
            let header = update_header(
                sql,
                company_id,
                &current,
                current.supplier_id,
                &current.currency_code,
                &amendable,
                amendment_number,
                &mut changes,
            )
            .await?;
            supersede_lines(sql, &current, amendment_id, &mut changes).await?;
            insert_lines(
                sql,
                tenant,
                company_id,
                current.id,
                Some(amendment_id),
                &valid_lines,
                &mut changes,
            )
            .await?;
            if resubmitted {
                transition_in(
                    sql,
                    tenant,
                    company_id,
                    &header,
                    OrderStatus::Submitted,
                    Stamps::Resubmitted,
                    actor,
                    Some(&reason),
                    &mut changes,
                )
                .await?;
            }
            Ok(((), changes))
        })
    })
    .await?;
    saved(tx, tenant, company_id, order_id).await
}

/// One line's new received quantity, judged by `judge` (the receipt's
/// ceiling or the reversal's floor), or the errors of every receipt line.
fn received_quantities(
    order: &Order,
    receipts: &[ReceiptLine],
    judge: impl Fn(&OrderLine, Decimal) -> Option<&'static str>,
    add: bool,
) -> std::result::Result<Vec<(OrderLine, Decimal)>, Vec<FieldError>> {
    let lines_error = |message: &str| vec![FieldError::new("lines", message)];
    if receipts.is_empty() {
        return Err(lines_error("needs at least one receipt"));
    }
    let mut seen = std::collections::HashSet::new();
    if !receipts.iter().all(|receipt| seen.insert(receipt.line_id)) {
        return Err(lines_error("can name each line only once"));
    }
    let mut checks = Checks::default();
    let mut applied = Vec::with_capacity(receipts.len());
    for (index, receipt) in receipts.iter().enumerate() {
        let number = index + 1;
        let Some(line) = order.lines.iter().find(|line| line.id == receipt.line_id) else {
            checks.take_line(
                number,
                Err::<(), _>(FieldError::new(
                    "line_id",
                    "is not a current line of this order",
                )),
            );
            continue;
        };
        if receipt.quantity <= Decimal::ZERO {
            checks.take_line(
                number,
                Err::<(), _>(FieldError::new("quantity", "must be greater than 0")),
            );
            continue;
        }
        let received = if add {
            line.received_quantity.checked_add(receipt.quantity)
        } else {
            line.received_quantity.checked_sub(receipt.quantity)
        };
        let message = match received {
            None => Some("is invalid"),
            Some(value) if value.round_dp(4) != value => Some("has more than 4 decimal places"),
            Some(value) => judge(line, value),
        };
        match (message, received) {
            (None, Some(value)) => applied.push((line.clone(), value)),
            (message, _) => {
                checks.take_line(
                    number,
                    Err::<(), _>(FieldError::new(
                        "received_quantity",
                        message.unwrap_or("is invalid"),
                    )),
                );
            }
        }
    }
    checks.finish().map(|()| applied)
}

/// The over-receipt tolerance the caller states: a percentage from 0 to 100
/// with up to four decimal places, as a tolerance setting stores it.
fn tolerance(value: Decimal) -> std::result::Result<Decimal, FieldError> {
    let field = "tolerance_percent";
    if value < Decimal::ZERO {
        Err(FieldError::new(field, "must be greater than or equal to 0"))
    } else if value > Decimal::ONE_HUNDRED {
        Err(FieldError::new(field, "must be less than or equal to 100"))
    } else if value.round_dp(4) != value {
        Err(FieldError::new(field, "has more than 4 decimal places"))
    } else {
        Ok(value)
    }
}

/// Goods received against a sent or partially received order: each receipt
/// line adds to a current line's received quantity, up to the ordered
/// quantity raised by `tolerance_percent`. A sent order becomes partially
/// received; one whose current lines are all received in full closes.
pub(crate) async fn record_receipt(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    order_id: i64,
    receipts: &[ReceiptLine],
    tolerance_percent: Decimal,
) -> Result<Outcome<Order>> {
    let tenant = scope.tenant();
    let order = refuse!(locked(tx, scope, company_id, order_id).await?);
    let receivable = [OrderStatus::Sent, OrderStatus::PartiallyReceived];
    refuse!(require_status(&order, &receivable, "received"));
    let tolerance =
        refuse!(tolerance(tolerance_percent).map_err(|error| Refusal::Invalid(vec![error])));
    let applied = refuse!(received_quantities(
        &order,
        receipts,
        |line, received| {
            let ceiling = line
                .quantity
                .saturating_mul(Decimal::ONE_HUNDRED + tolerance)
                / Decimal::ONE_HUNDRED;
            if received <= ceiling {
                None
            } else if ceiling == line.quantity {
                Some("exceeds the ordered quantity")
            } else {
                Some("exceeds the ordered quantity and the tolerance")
            }
        },
        true,
    )
    .map_err(Refusal::Invalid));
    let actor = actor(scope);
    tx.mutate(move |sql| {
        Box::pin(async move {
            let mut changes = Vec::new();
            let mut lines = order.lines.clone();
            for (line, received) in &applied {
                let updated = set_received(sql, order.id, line, *received, &mut changes).await?;
                if let Some(slot) = lines.iter_mut().find(|slot| slot.id == updated.id) {
                    *slot = updated;
                }
            }
            if lines
                .iter()
                .all(|line| line.received_quantity >= line.quantity)
            {
                transition_in(
                    sql,
                    tenant,
                    company_id,
                    &order,
                    OrderStatus::Closed,
                    Stamps::Closed,
                    actor,
                    Some("every line received in full"),
                    &mut changes,
                )
                .await?;
            } else if order.status == OrderStatus::Sent {
                transition_in(
                    sql,
                    tenant,
                    company_id,
                    &order,
                    OrderStatus::PartiallyReceived,
                    Stamps::None,
                    actor,
                    None,
                    &mut changes,
                )
                .await?;
            }
            Ok(((), changes))
        })
    })
    .await?;
    saved(tx, tenant, company_id, order_id).await
}

/// Goods taken back when a receipt is reversed: each line subtracts from a
/// current line's received quantity, never below zero, with a reason. A
/// closed order goes back to partially received with the reason in its
/// history.
pub(crate) async fn reverse_receipt(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    order_id: i64,
    receipts: &[ReceiptLine],
    reason_text: &str,
) -> Result<Outcome<Order>> {
    let tenant = scope.tenant();
    let order = refuse!(locked(tx, scope, company_id, order_id).await?);
    let reversible = [OrderStatus::PartiallyReceived, OrderStatus::Closed];
    refuse!(require_status(&order, &reversible, "partially_received"));
    let reason = refuse!(reason(reason_text, true).map_err(|error| Refusal::Invalid(vec![error])));
    let applied = refuse!(received_quantities(
        &order,
        receipts,
        |_, received| (received < Decimal::ZERO).then_some("exceeds what was received"),
        false,
    )
    .map_err(Refusal::Invalid));
    let actor = actor(scope);
    tx.mutate(move |sql| {
        Box::pin(async move {
            let mut changes = Vec::new();
            // The lines change after the transition, which the line rules read.
            if order.status == OrderStatus::Closed {
                transition_in(
                    sql,
                    tenant,
                    company_id,
                    &order,
                    OrderStatus::PartiallyReceived,
                    Stamps::Reopened,
                    actor,
                    reason.as_deref(),
                    &mut changes,
                )
                .await?;
            }
            for (line, received) in &applied {
                set_received(sql, order.id, line, *received, &mut changes).await?;
            }
            Ok(((), changes))
        })
    })
    .await?;
    saved(tx, tenant, company_id, order_id).await
}
