//! A purchase order's lifecycle (Bilimbi `create_order/3`,
//! `create_order_from_request/4`, `update_order/4` and the transitions):
//! draft, submitted, approved, sent, closed or cancelled, with a submitted
//! order returned to draft for correction. Every creation and transition
//! writes its history row in the same mutation. Private to the facade.
use crate::orders::{
    active_supplier, actor, currency_code, delete_draft_lines, insert_event, insert_header,
    insert_lines, locked, require_status, saved, transition, update_header, validate_lines,
    Amendable, Stamps,
};
use crate::validate::{reason, Checks, BLANK};
use crate::{
    authorize, requests, FieldError, NewOrder, NewOrderLine, Order, OrderChanges, OrderStatus,
    Outcome, Refusal, RequestStatus, APPROVE_ORDERS,
};
use bilibi_database::{Change, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use serde_json::json;

/// Answers the refusal of a `Result<_, Refusal>` step as the outcome.
macro_rules! refuse {
    ($step:expr) => {
        match $step {
            Ok(value) => value,
            Err(refusal) => return Ok(Outcome::Refused(refusal)),
        }
    };
}
pub(crate) use refuse;

/// Bilimbi `insert_order/4`: the supplier first (a refusal of its own), then
/// every field and line, then the header, lines and creation history row in
/// one mutation.
async fn insert(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    input: &NewOrder,
    request_id: Option<i64>,
) -> Result<Outcome<Order>> {
    let tenant = scope.tenant();
    if let Some(supplier_id) = input.supplier_id {
        refuse!(active_supplier(tx, tenant, company_id, supplier_id).await?);
    }
    let mut checks = Checks::default();
    let supplier_id = checks.take(
        input
            .supplier_id
            .ok_or_else(|| FieldError::new("supplier_id", BLANK)),
    );
    let currency_code = checks.take(currency_code(&input.currency_code));
    let mut amendable = Amendable::default();
    amendable.apply(
        &mut checks,
        Some(&input.reference),
        Some(&input.notes),
        Some(&input.expected_on),
    );
    let valid_lines =
        validate_lines(tx.sql(), tenant, company_id, &mut checks, &input.lines).await?;
    if let Err(errors) = checks.finish() {
        return Ok(Outcome::Refused(Refusal::Invalid(errors)));
    }
    let actor = actor(scope);
    let id = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let mut order = insert_header(
                    sql,
                    tenant,
                    company_id,
                    request_id,
                    supplier_id,
                    &currency_code,
                    &amendable,
                    actor,
                )
                .await?;
                let mut changes = Vec::new();
                order.lines = insert_lines(
                    sql,
                    tenant,
                    company_id,
                    order.id,
                    None,
                    &valid_lines,
                    &mut changes,
                )
                .await?;
                changes
                    .push(insert_event(sql, tenant, company_id, &order, None, actor, None).await?);
                changes.insert(
                    0,
                    Change {
                        kind: "supply_chain.purchasing.order".into(),
                        id: order.id,
                        old: json!({}),
                        new: crate::orders::to_json(&order),
                    },
                );
                Ok((order.id, changes))
            })
        })
        .await?;
    saved(tx, tenant, company_id, id).await
}

pub(crate) async fn create(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    input: &NewOrder,
) -> Result<Outcome<Order>> {
    insert(tx, scope, company_id, input, None).await
}

/// An approved request ordered, perhaps more than once (from two suppliers,
/// say); without `lines` its lines are copied at a unit price of zero.
pub(crate) async fn create_from_request(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    request_id: i64,
    input: &crate::OrderFromRequest,
) -> Result<Outcome<Order>> {
    let request = refuse!(requests::locked(tx, scope, company_id, request_id).await?);
    if request.status != RequestStatus::Approved {
        return Ok(Outcome::Refused(Refusal::InvalidTransition {
            status: request.status.as_str(),
            target: "ordered",
        }));
    }
    let lines = match &input.lines {
        Some(lines) => lines.clone(),
        None => request
            .lines
            .iter()
            .map(|line| NewOrderLine {
                description: line.description.clone(),
                external_item_ref: line.external_item_ref.clone().unwrap_or_default(),
                quantity: line.quantity.to_string(),
                unit: line.unit.clone(),
                unit_price: "0".into(),
                tax_code_id: None,
            })
            .collect(),
    };
    let order = NewOrder {
        supplier_id: input.supplier_id,
        currency_code: input.currency_code.clone(),
        reference: input.reference.clone(),
        notes: input.notes.clone(),
        expected_on: input.expected_on.clone(),
        lines,
    };
    insert(tx, scope, company_id, &order, Some(request.id)).await
}

pub(crate) async fn update(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    order_id: i64,
    input: &OrderChanges,
) -> Result<Outcome<Order>> {
    let tenant = scope.tenant();
    let current = refuse!(locked(tx, scope, company_id, order_id).await?);
    refuse!(require_status(&current, &[OrderStatus::Draft], "draft"));
    // The supplier the draft will name, given or kept, is still active.
    let supplier_id = input.supplier_id.unwrap_or(current.supplier_id);
    refuse!(active_supplier(tx, tenant, company_id, supplier_id).await?);
    let mut checks = Checks::default();
    let currency_code = match &input.currency_code {
        Some(value) => checks.take(currency_code(value)),
        None => current.currency_code.clone(),
    };
    let mut amendable = Amendable::of(&current);
    amendable.apply(
        &mut checks,
        input.reference.as_deref(),
        input.notes.as_deref(),
        input.expected_on.as_deref(),
    );
    let valid_lines = match &input.lines {
        Some(lines) => {
            Some(validate_lines(tx.sql(), tenant, company_id, &mut checks, lines).await?)
        }
        None => None,
    };
    if let Err(errors) = checks.finish() {
        return Ok(Outcome::Refused(Refusal::Invalid(errors)));
    }
    tx.mutate(move |sql| {
        Box::pin(async move {
            let mut changes = Vec::new();
            update_header(
                sql,
                company_id,
                &current,
                supplier_id,
                &currency_code,
                &amendable,
                current.amendment_count,
                &mut changes,
            )
            .await?;
            if let Some(valid_lines) = valid_lines {
                delete_draft_lines(sql, &current, &mut changes).await?;
                insert_lines(
                    sql,
                    tenant,
                    company_id,
                    current.id,
                    None,
                    &valid_lines,
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

/// Draft to submitted: at least one line and an active supplier.
pub(crate) async fn submit(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    order_id: i64,
) -> Result<Outcome<Order>> {
    let order = refuse!(locked(tx, scope, company_id, order_id).await?);
    refuse!(require_status(&order, &[OrderStatus::Draft], "submitted"));
    if order.lines.is_empty() {
        return Ok(Outcome::Refused(Refusal::NoLines));
    }
    if active_supplier(tx, scope.tenant(), company_id, order.supplier_id)
        .await?
        .is_err()
    {
        return Ok(Outcome::Refused(Refusal::SupplierInactive));
    }
    let to = OrderStatus::Submitted;
    transition(tx, scope, company_id, order, to, Stamps::Submitted, None).await
}

/// Submitted to approved, by someone other than its submitter.
pub(crate) async fn approve(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    order_id: i64,
    reason_text: &str,
) -> Result<Outcome<Order>> {
    let order = refuse!(locked(tx, scope, company_id, order_id).await?);
    refuse!(require_status(
        &order,
        &[OrderStatus::Submitted],
        "approved"
    ));
    if order.submitted_by_user_id == Some(scope.user()) {
        return Ok(Outcome::Refused(Refusal::OwnOrder));
    }
    let reason = refuse!(reason(reason_text, false).map_err(|e| Refusal::Invalid(vec![e])));
    let to = OrderStatus::Approved;
    transition(tx, scope, company_id, order, to, Stamps::Approved, reason).await
}

/// Submitted back to draft with a reason; an amended order keeps its trail,
/// so it is approved or cancelled instead.
pub(crate) async fn return_to_draft(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    order_id: i64,
    reason_text: &str,
) -> Result<Outcome<Order>> {
    let order = refuse!(locked(tx, scope, company_id, order_id).await?);
    refuse!(require_status(&order, &[OrderStatus::Submitted], "draft"));
    if order.amendment_count != 0 {
        return Ok(Outcome::Refused(Refusal::InvalidTransition {
            status: order.status.as_str(),
            target: "draft",
        }));
    }
    let reason = refuse!(reason(reason_text, true).map_err(|e| Refusal::Invalid(vec![e])));
    let to = OrderStatus::Draft;
    transition(tx, scope, company_id, order, to, Stamps::Returned, reason).await
}

pub(crate) async fn send(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    order_id: i64,
) -> Result<Outcome<Order>> {
    let order = refuse!(locked(tx, scope, company_id, order_id).await?);
    refuse!(require_status(&order, &[OrderStatus::Approved], "sent"));
    let to = OrderStatus::Sent;
    transition(tx, scope, company_id, order, to, Stamps::Sent, None).await
}

/// A sent or partially received order closed short, with a reason.
pub(crate) async fn close(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    order_id: i64,
    reason_text: &str,
) -> Result<Outcome<Order>> {
    let order = refuse!(locked(tx, scope, company_id, order_id).await?);
    let receivable = [OrderStatus::Sent, OrderStatus::PartiallyReceived];
    refuse!(require_status(&order, &receivable, "closed"));
    let reason = refuse!(reason(reason_text, true).map_err(|e| Refusal::Invalid(vec![e])));
    let to = OrderStatus::Closed;
    transition(tx, scope, company_id, order, to, Stamps::Closed, reason).await
}

/// Any open order cancelled with a reason, never with goods received
/// against it. Cancelling an order that was ever approved undoes an
/// approver's decision, so it takes the approver's capability as well.
pub(crate) async fn cancel(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    order_id: i64,
    reason_text: &str,
) -> Result<Outcome<Order>> {
    let order = refuse!(locked(tx, scope, company_id, order_id).await?);
    let open = [
        OrderStatus::Draft,
        OrderStatus::Submitted,
        OrderStatus::Approved,
        OrderStatus::Sent,
        OrderStatus::PartiallyReceived,
    ];
    refuse!(require_status(&order, &open, "cancelled"));
    if order
        .lines
        .iter()
        .any(|line| line.received_quantity > rust_decimal::Decimal::ZERO)
    {
        return Ok(Outcome::Refused(Refusal::GoodsReceived));
    }
    let never_approved = matches!(order.status, OrderStatus::Draft | OrderStatus::Submitted)
        && order.amendment_count == 0;
    if !never_approved {
        authorize(tx.sql(), scope, company_id, APPROVE_ORDERS).await?;
    }
    let reason = refuse!(reason(reason_text, true).map_err(|e| Refusal::Invalid(vec![e])));
    let to = OrderStatus::Cancelled;
    transition(tx, scope, company_id, order, to, Stamps::Cancelled, reason).await
}
