//! Bilimbi Receiving writes. The facade owns commit/rollback; these functions
//! never open a transaction or reach Purchasing except through its facade.
use crate::records::{self, change, header_json, HEADER};
use crate::validate::{self, Checks};
use crate::*;
use bilibi_events::NewEvent;
use bilibi_supply_chain_purchasing::{Order, OrderStatus, Outcome as PurchaseOutcome};
use rust_decimal::Decimal;
use serde_json::json;

macro_rules! refuse {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(reason) => return Ok(Outcome::Refused(reason)),
        }
    };
}

pub(crate) async fn configure(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company: i64,
    percent: &str,
) -> Result<Outcome<Settings>> {
    let percent = refuse!(validate::tolerance(percent).map_err(|e| Refusal::Invalid(vec![e])));
    let scope = scope.clone();
    tx.mutate(move |sql| Box::pin(async move {
        let row = sql.query_one("INSERT INTO supply_chain_receiving_settings(tenant_id,company_id,over_receipt_tolerance_percent,set_by_user_id,set_by_impersonator_id) VALUES($1,$2,$3,$4,$5) RETURNING id", &[&scope.tenant(), &company, &percent, &scope.user(), &scope.impersonator().map(|i| i.user())]).await?;
        let id = row.try_get(0)?;
        Ok(((), vec![change("setting", id, json!({}), json!({"id":id,"over_receipt_tolerance_percent":percent.to_string(),"set_by_user_id":scope.user(),"set_by_impersonator_id":scope.impersonator().map(|i| i.user())}))]))
    })).await?;
    Ok(Outcome::Saved(
        records::settings(tx.sql(), tx.context().tenant, company).await?,
    ))
}

async fn order(
    service: &Receiving,
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    company: i64,
    id: i64,
) -> Result<std::result::Result<Order, Refusal>> {
    Ok(
        match service
            .purchasing
            .get_order_on(tx.sql(), scope, company, id)
            .await?
        {
            None => Err(Refusal::OrderNotFound),
            Some(order)
                if matches!(
                    order.status,
                    OrderStatus::Sent | OrderStatus::PartiallyReceived
                ) =>
            {
                Ok(order)
            }
            Some(order) => Err(Refusal::OrderNotReceivable(order.status.as_str())),
        },
    )
}
fn draft(receipt: &Receipt, target: &'static str) -> std::result::Result<(), Refusal> {
    if receipt.status == ReceiptStatus::Draft {
        Ok(())
    } else {
        Err(Refusal::InvalidTransition {
            status: receipt.status.as_str(),
            target,
        })
    }
}

#[derive(Default)]
struct Header {
    received_on: Option<String>,
    supplier_reference: Option<String>,
    notes: Option<String>,
}
impl Header {
    fn checked(
        date: Option<&str>,
        reference: &str,
        notes: &str,
    ) -> std::result::Result<Self, Refusal> {
        let mut checks = Checks::default();
        let valid = Self {
            received_on: date.map(|d| checks.take(validate::date(d))),
            supplier_reference: checks.take(validate::text(
                "supplier_reference",
                reference,
                100,
                false,
            )),
            notes: checks.take(validate::text("notes", notes, 1000, false)),
        };
        checks.finish()?;
        Ok(valid)
    }
}
struct Draft {
    order_id: i64,
    order_number: i64,
    supplier_id: i64,
    reverses: Option<i64>,
    reason: Option<String>,
    header: Header,
    lines: Vec<ReceiptLine>,
}
async fn insert(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company: i64,
    draft: Draft,
) -> Result<Receipt> {
    let scope = scope.clone();
    let id = tx.mutate(move |sql| Box::pin(async move {
        let kind = if draft.reverses.is_some() { "reversal" } else { "receipt" };
        let h = &draft.header;
        let row = sql.query_one(&format!("INSERT INTO supply_chain_receiving_receipts AS r(tenant_id,company_id,kind,order_id,order_number,supplier_id,reverses_receipt_id,reason,received_on,supplier_reference,notes,created_by_user_id,created_by_impersonator_id,inserted_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,COALESCE($9::text::date,(now() AT TIME ZONE 'UTC')::date),$10,$11,$12,$13,now() AT TIME ZONE 'UTC',now() AT TIME ZONE 'UTC') RETURNING {HEADER}"), &[&scope.tenant(), &company, &kind, &draft.order_id, &draft.order_number, &draft.supplier_id, &draft.reverses, &draft.reason, &h.received_on, &h.supplier_reference, &h.notes, &scope.user(), &scope.impersonator().map(|i| i.user())]).await?;
        let receipt = records::header(&row)?;
        let mut changes = vec![change("receipt", receipt.id, json!({}), header_json(&receipt))];
        records::insert_lines(sql, &scope, company, receipt.id, &draft.lines, &mut changes).await?;
        changes.push(records::insert_event(sql, &scope, company, &receipt, None, None).await?);
        Ok((receipt.id, changes))
    })).await?;
    records::get(tx.sql(), tx.context().tenant, company, id, true)
        .await?
        .ok_or(Error::Internal)
}

pub(crate) async fn create(
    service: &Receiving,
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company: i64,
    input: &NewReceipt,
) -> Result<Outcome<Receipt>> {
    let order = refuse!(order(service, tx, scope, company, input.order_id).await?);
    let tolerance = records::settings(tx.sql(), scope.tenant(), company)
        .await?
        .over_receipt_tolerance_percent;
    let lines = refuse!(validate::lines(&order, &input.lines, tolerance));
    let header = refuse!(Header::checked(
        input.received_on.as_deref(),
        &input.supplier_reference,
        &input.notes
    ));
    Ok(Outcome::Saved(
        insert(
            tx,
            scope,
            company,
            Draft {
                order_id: order.id,
                order_number: order.order_number,
                supplier_id: order.supplier_id,
                reverses: None,
                reason: None,
                header,
                lines,
            },
        )
        .await?,
    ))
}

pub(crate) async fn update(
    service: &Receiving,
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company: i64,
    id: i64,
    input: &ReceiptChanges,
) -> Result<Outcome<Receipt>> {
    let current = refuse!(records::locked(tx, scope, company, id)
        .await?
        .ok_or(Refusal::ReceiptNotFound));
    refuse!(draft(&current, "draft"));
    let header = refuse!(Header::checked(
        Some(input.received_on.as_deref().unwrap_or(&current.received_on)),
        input
            .supplier_reference
            .as_deref()
            .unwrap_or(current.supplier_reference.as_deref().unwrap_or("")),
        input
            .notes
            .as_deref()
            .unwrap_or(current.notes.as_deref().unwrap_or("")),
    ));
    let lines = if let Some(inputs) = &input.lines {
        if inputs.is_empty() {
            return Ok(Outcome::Refused(Refusal::NoLines));
        }
        let order = refuse!(order(service, tx, scope, company, current.order_id).await?);
        let tolerance = records::settings(tx.sql(), scope.tenant(), company)
            .await?
            .over_receipt_tolerance_percent;
        Some(refuse!(validate::lines(&order, inputs, tolerance)))
    } else {
        None
    };
    let scope = scope.clone();
    tx.mutate(move |sql| Box::pin(async move {
        let h = header;
        let row = sql.query_one(&format!("UPDATE supply_chain_receiving_receipts AS r SET received_on=$4::text::date,supplier_reference=$5,notes=$6,updated_at=now() AT TIME ZONE 'UTC' WHERE tenant_id=$1 AND company_id=$2 AND id=$3 RETURNING {HEADER}"), &[&scope.tenant(), &company, &id, &h.received_on, &h.supplier_reference, &h.notes]).await?;
        let updated = records::header(&row)?;
        let mut changes = vec![change("receipt", id, header_json(&current), header_json(&updated))];
        if let Some(lines) = lines { records::replace_lines(sql, &scope, company, &current, &lines, &mut changes).await?; }
        Ok(((), changes))
    })).await?;
    Ok(Outcome::Saved(
        records::get(tx.sql(), tx.context().tenant, company, id, true)
            .await?
            .ok_or(Error::Internal)?,
    ))
}

pub(crate) async fn cancel(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company: i64,
    id: i64,
    reason: &str,
) -> Result<Outcome<Receipt>> {
    let current = refuse!(records::locked(tx, scope, company, id)
        .await?
        .ok_or(Refusal::ReceiptNotFound));
    refuse!(draft(&current, "cancelled"));
    let reason =
        refuse!(validate::text("reason", reason, 500, true).map_err(|e| Refusal::Invalid(vec![e])));
    Ok(Outcome::Saved(
        records::transition(
            tx,
            scope,
            company,
            current,
            ReceiptStatus::Cancelled,
            reason,
        )
        .await?,
    ))
}

fn accepted(receipt: &Receipt) -> Vec<bilibi_supply_chain_purchasing::ReceiptLine> {
    receipt
        .lines
        .iter()
        .filter(|l| l.received_quantity > Decimal::ZERO)
        .map(|l| bilibi_supply_chain_purchasing::ReceiptLine {
            line_id: l.order_line_id,
            quantity: l.received_quantity,
        })
        .collect()
}
fn purchasing(result: PurchaseOutcome<Order>) -> std::result::Result<(), Refusal> {
    match result {
        PurchaseOutcome::Saved(_) => Ok(()),
        PurchaseOutcome::Refused(reason) => Err(Refusal::Purchasing(reason)),
        // The writable company is already locked by Receiving; its state
        // cannot become archived halfway through this transaction.
        PurchaseOutcome::CompanyArchived => unreachable!("company lock held"),
    }
}

pub(crate) async fn post(
    service: &Receiving,
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company: i64,
    id: i64,
) -> Result<Outcome<Receipt>> {
    let current = refuse!(records::locked(tx, scope, company, id)
        .await?
        .ok_or(Refusal::ReceiptNotFound));
    refuse!(draft(&current, "posted"));
    let order = refuse!(order(service, tx, scope, company, current.order_id).await?);
    let mut refreshed = current.lines.clone();
    let mut errors = Vec::new();
    for (i, l) in refreshed.iter_mut().enumerate() {
        if let Some(source) = order.lines.iter().find(|s| s.id == l.order_line_id) {
            l.order_line_number = source.line_number;
            l.description = source.description.clone();
            l.unit = source.unit.clone();
            l.ordered_quantity = source.quantity;
        } else {
            errors.push(FieldError {
                field: "order_line_id",
                line: Some(i + 1),
                message: "is not a current line of this order".into(),
            });
        }
    }
    if !errors.is_empty() {
        return Ok(Outcome::Refused(Refusal::Invalid(errors)));
    }
    let accepted = accepted(&current);
    if !accepted.is_empty() {
        let tolerance = records::settings(tx.sql(), scope.tenant(), company)
            .await?
            .over_receipt_tolerance_percent;
        refuse!(purchasing(
            service
                .purchasing
                .record_receipt_in(tx, scope, company, current.order_id, &accepted, tolerance)
                .await?
        ));
    }
    let scope_copy = scope.clone();
    let copy = current.clone();
    tx.mutate(move |sql| {
        Box::pin(async move {
            let mut changes = Vec::new();
            records::refresh_lines(sql, &scope_copy, company, &copy, &refreshed, &mut changes)
                .await?;
            Ok(((), changes))
        })
    })
    .await?;
    let posted =
        records::transition(tx, scope, company, current, ReceiptStatus::Posted, None).await?;
    publish(service, tx, company, &posted).await?;
    Ok(Outcome::Saved(posted))
}

pub(crate) async fn reverse(
    service: &Receiving,
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company: i64,
    id: i64,
    input: &Reversal,
) -> Result<Outcome<Receipt>> {
    let original = refuse!(records::locked(tx, scope, company, id)
        .await?
        .ok_or(Refusal::ReceiptNotFound));
    if original.status != ReceiptStatus::Posted {
        return Ok(Outcome::Refused(Refusal::InvalidTransition {
            status: original.status.as_str(),
            target: "reversed",
        }));
    }
    if original.kind == ReceiptKind::Reversal {
        return Ok(Outcome::Refused(Refusal::ReversalNotReversible));
    }
    if original.reversed_by_receipt_id.is_some() {
        return Ok(Outcome::Refused(Refusal::ReceiptReversed));
    }
    let reason =
        refuse!(validate::text("reason", &input.reason, 500, true)
            .map_err(|e| Refusal::Invalid(vec![e])));
    let header = refuse!(Header::checked(
        input.received_on.as_deref(),
        original.supplier_reference.as_deref().unwrap_or(""),
        &input.notes
    ));
    let reversal = insert(
        tx,
        scope,
        company,
        Draft {
            order_id: original.order_id,
            order_number: original.order_number,
            supplier_id: original.supplier_id,
            reverses: Some(id),
            reason: reason.clone(),
            header,
            lines: original.lines.clone(),
        },
    )
    .await?;
    let accepted = accepted(&original);
    if !accepted.is_empty() {
        refuse!(purchasing(
            service
                .purchasing
                .reverse_receipt_in(
                    tx,
                    scope,
                    company,
                    original.order_id,
                    &accepted,
                    reason.as_deref().unwrap_or("")
                )
                .await?
        ));
    }
    let posted =
        records::transition(tx, scope, company, reversal, ReceiptStatus::Posted, reason).await?;
    publish(service, tx, company, &posted).await?;
    Ok(Outcome::Saved(posted))
}

/// One independently consumable line fact per event avoids imposing the
/// Platform's 100-member/64-KiB event bounds on a valid multi-line receipt.
async fn publish(
    service: &Receiving,
    tx: &mut MutationTransaction<'_>,
    company: i64,
    receipt: &Receipt,
) -> Result<()> {
    let posting = receipt
        .events
        .iter()
        .find(|e| e.to_status == ReceiptStatus::Posted)
        .ok_or(Error::Internal)?;
    let row = tx.sql().query_one("SELECT to_char(occurred_at,'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM supply_chain_receiving_receipt_events WHERE tenant_id=$1 AND company_id=$2 AND id=$3", &[&tx.context().tenant, &company, &posting.id]).await?;
    let posted_at: String = row.try_get(0)?;
    let name = if receipt.kind == ReceiptKind::Receipt {
        GOODS_RECEIVED
    } else {
        RECEIPT_REVERSED
    };
    let subject = format!("order:{}", receipt.order_id);
    for line in &receipt.lines {
        let key = format!("posting:{}:line:{}", posting.id, line.line_number);
        service.events.record(tx, NewEvent {
            name, subject: &subject, key: &key,
            payload: json!({
                "company_id":company,"posting_id":posting.id,"posted_at":posted_at,
                "receipt_id":receipt.id,"receipt_number":receipt.receipt_number,"kind":receipt.kind.as_str(),
                "order_id":receipt.order_id,"order_number":receipt.order_number,"supplier_id":receipt.supplier_id,
                "reverses_receipt_id":receipt.reverses_receipt_id,"reason":receipt.reason,
                "received_on":receipt.received_on,"supplier_reference":receipt.supplier_reference,"notes":receipt.notes,
                "part_index":line.line_number,"part_count":receipt.lines.len(),"line":records::line_json(line),
            }),
        }).await?;
    }
    Ok(())
}
