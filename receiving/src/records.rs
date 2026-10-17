//! Bilimbi Receiving SQL and schema-free views. No Purchasing table access.
use crate::*;
use bilibi_database::Change;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio_postgres::{Row, Transaction};

pub(crate) const HEADER: &str = "r.id,r.receipt_number,r.kind,r.status,r.order_id,r.order_number,r.supplier_id,r.reverses_receipt_id,r.reason,to_char(r.received_on,'YYYY-MM-DD'),r.supplier_reference,r.notes,r.created_by_user_id,r.created_by_impersonator_id,r.posted_by_user_id,r.posted_by_impersonator_id,r.posted_at,r.cancelled_at,r.inserted_at,r.updated_at";
const LINE: &str = "l.id,l.line_number,l.order_line_id,l.order_line_number,l.description,l.unit,l.ordered_quantity,l.received_quantity,l.rejected_quantity,l.rejection_reason";

fn status(value: &str) -> Result<ReceiptStatus> {
    ReceiptStatus::parse(value).ok_or(Error::Internal)
}
fn kind(value: &str) -> Result<ReceiptKind> {
    match value {
        "receipt" => Ok(ReceiptKind::Receipt),
        "reversal" => Ok(ReceiptKind::Reversal),
        _ => Err(Error::Internal),
    }
}
pub(crate) fn header(row: &Row) -> Result<Receipt> {
    Ok(Receipt {
        id: row.try_get(0)?,
        receipt_number: row.try_get(1)?,
        kind: kind(row.try_get(2)?)?,
        status: status(row.try_get(3)?)?,
        order_id: row.try_get(4)?,
        order_number: row.try_get(5)?,
        supplier_id: row.try_get(6)?,
        reverses_receipt_id: row.try_get(7)?,
        reason: row.try_get(8)?,
        received_on: row.try_get(9)?,
        supplier_reference: row.try_get(10)?,
        notes: row.try_get(11)?,
        created_by_user_id: row.try_get(12)?,
        created_by_impersonator_id: row.try_get(13)?,
        posted_by_user_id: row.try_get(14)?,
        posted_by_impersonator_id: row.try_get(15)?,
        posted_at: row.try_get(16)?,
        cancelled_at: row.try_get(17)?,
        inserted_at: row.try_get(18)?,
        updated_at: row.try_get(19)?,
        reverses_receipt_number: None,
        reversed_by_receipt_id: None,
        reversed_by_receipt_number: None,
        lines: Vec::new(),
        events: Vec::new(),
    })
}
fn line(row: &Row) -> Result<ReceiptLine> {
    Ok(ReceiptLine {
        id: row.try_get(0)?,
        line_number: row.try_get(1)?,
        order_line_id: row.try_get(2)?,
        order_line_number: row.try_get(3)?,
        description: row.try_get(4)?,
        unit: row.try_get(5)?,
        ordered_quantity: row.try_get(6)?,
        received_quantity: row.try_get(7)?,
        rejected_quantity: row.try_get(8)?,
        rejection_reason: row.try_get(9)?,
    })
}
fn event(row: &Row) -> Result<ReceiptEvent> {
    Ok(ReceiptEvent {
        id: row.try_get(0)?,
        from_status: row.try_get::<_, Option<&str>>(1)?.map(status).transpose()?,
        to_status: status(row.try_get(2)?)?,
        actor_user_id: row.try_get(3)?,
        actor_impersonator_id: row.try_get(4)?,
        reason: row.try_get(5)?,
        occurred_at: row.try_get(6)?,
    })
}

async fn load(
    c: &impl GenericClient,
    tenant: i64,
    company: i64,
    rows: &[Row],
    history: bool,
) -> Result<Vec<Receipt>> {
    let mut receipts = rows.iter().map(header).collect::<Result<Vec<_>>>()?;
    let ids: Vec<_> = receipts.iter().map(|r| r.id).collect();
    if ids.is_empty() {
        return Ok(receipts);
    }
    let positions: HashMap<_, _> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let lines = c.query(&format!("SELECT {LINE},l.receipt_id FROM supply_chain_receiving_receipt_lines l WHERE l.tenant_id=$1 AND l.company_id=$2 AND l.receipt_id=ANY($3) ORDER BY l.line_number"), &[&tenant, &company, &ids]).await?;
    for row in &lines {
        let id: i64 = row.try_get(10)?;
        if let Some(i) = positions.get(&id) {
            receipts[*i].lines.push(line(row)?);
        }
    }
    let links = c.query("SELECT r.id,o.receipt_number,v.id,v.receipt_number FROM supply_chain_receiving_receipts r LEFT JOIN supply_chain_receiving_receipts o ON o.tenant_id=r.tenant_id AND o.company_id=r.company_id AND o.id=r.reverses_receipt_id LEFT JOIN supply_chain_receiving_receipts v ON v.tenant_id=r.tenant_id AND v.company_id=r.company_id AND v.reverses_receipt_id=r.id AND v.status='posted' WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.id=ANY($3)", &[&tenant, &company, &ids]).await?;
    for row in &links {
        let id: i64 = row.try_get(0)?;
        if let Some(i) = positions.get(&id) {
            receipts[*i].reverses_receipt_number = row.try_get(1)?;
            receipts[*i].reversed_by_receipt_id = row.try_get(2)?;
            receipts[*i].reversed_by_receipt_number = row.try_get(3)?;
        }
    }
    if history {
        let events = c.query("SELECT id,from_status,to_status,actor_user_id,actor_impersonator_id,reason,occurred_at,receipt_id FROM supply_chain_receiving_receipt_events WHERE tenant_id=$1 AND company_id=$2 AND receipt_id=ANY($3) ORDER BY id", &[&tenant, &company, &ids]).await?;
        for row in &events {
            let id: i64 = row.try_get(7)?;
            if let Some(i) = positions.get(&id) {
                receipts[*i].events.push(event(row)?);
            }
        }
    }
    Ok(receipts)
}

pub(crate) async fn get(
    c: &impl GenericClient,
    tenant: i64,
    company: i64,
    id: i64,
    history: bool,
) -> Result<Option<Receipt>> {
    let rows = c.query(&format!("SELECT {HEADER} FROM supply_chain_receiving_receipts r WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.id=$3"), &[&tenant, &company, &id]).await?;
    Ok(load(c, tenant, company, &rows, history).await?.pop())
}
pub(crate) async fn locked(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    company: i64,
    id: i64,
) -> Result<Option<Receipt>> {
    tx.sql().query_opt("SELECT id FROM supply_chain_receiving_receipts WHERE tenant_id=$1 AND company_id=$2 AND id=$3 FOR UPDATE", &[&scope.tenant(), &company, &id]).await?;
    get(tx.sql(), scope.tenant(), company, id, true).await
}
pub(crate) async fn list(
    c: &impl GenericClient,
    tenant: i64,
    company: i64,
    query: ReceiptQuery,
) -> Result<Vec<Receipt>> {
    let rows = c.query(&format!("SELECT {HEADER} FROM supply_chain_receiving_receipts r WHERE r.tenant_id=$1 AND r.company_id=$2 AND ($3::text IS NULL OR r.status=$3) AND ($4::bigint IS NULL OR r.order_id=$4) ORDER BY r.receipt_number DESC LIMIT $5"), &[&tenant, &company, &query.status.map(ReceiptStatus::as_str), &query.order_id, &query.limit]).await?;
    load(c, tenant, company, &rows, false).await
}
pub(crate) async fn postings(
    c: &impl GenericClient,
    tenant: i64,
    company: i64,
    query: PostingQuery,
) -> Result<Vec<Posting>> {
    let rows = c.query("SELECT id,occurred_at,receipt_id FROM supply_chain_receiving_receipt_events WHERE tenant_id=$1 AND company_id=$2 AND to_status='posted' AND id>$3 ORDER BY id LIMIT $4", &[&tenant, &company, &query.after_posting_id.unwrap_or(0), &query.limit]).await?;
    let ids = rows
        .iter()
        .map(|r| r.try_get::<_, i64>(2))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let headers = c.query(&format!("SELECT {HEADER} FROM supply_chain_receiving_receipts r WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.id=ANY($3)"), &[&tenant, &company, &ids]).await?;
    let mut receipts: HashMap<_, _> = load(c, tenant, company, &headers, false)
        .await?
        .into_iter()
        .map(|r| (r.id, r))
        .collect();
    rows.iter()
        .map(|r| {
            Ok(Posting {
                posting_id: r.try_get(0)?,
                posted_at: r.try_get(1)?,
                receipt: receipts
                    .remove(&r.try_get::<_, i64>(2)?)
                    .ok_or(Error::Internal)?,
            })
        })
        .collect()
}

pub(crate) async fn settings(
    c: &impl GenericClient,
    tenant: i64,
    company: i64,
) -> Result<Settings> {
    match c.query_opt("SELECT over_receipt_tolerance_percent,set_by_user_id,set_by_impersonator_id,occurred_at FROM supply_chain_receiving_settings WHERE tenant_id=$1 AND company_id=$2 ORDER BY id DESC LIMIT 1", &[&tenant, &company]).await? {
        Some(row) => Ok(Settings { over_receipt_tolerance_percent: row.try_get(0)?, set_by_user_id: row.try_get(1)?, set_by_impersonator_id: row.try_get(2)?, occurred_at: row.try_get(3)? }),
        None => Ok(Settings::default()),
    }
}

pub(crate) async fn summaries(
    c: &impl GenericClient,
    tenant: i64,
    company: i64,
    order: Option<i64>,
    order_line: Option<i64>,
) -> Result<Vec<OrderLineReceipts>> {
    let rows = c.query(&format!("SELECT {LINE},r.id,r.receipt_number,r.kind,r.reverses_receipt_id,to_char(r.received_on,'YYYY-MM-DD'),r.posted_at,r.order_id,r.order_number FROM supply_chain_receiving_receipt_lines l JOIN supply_chain_receiving_receipts r ON r.tenant_id=l.tenant_id AND r.company_id=l.company_id AND r.id=l.receipt_id WHERE r.tenant_id=$1 AND r.company_id=$2 AND r.status='posted' AND ($3::bigint IS NULL OR r.order_id=$3) AND ($4::bigint IS NULL OR l.order_line_id=$4) ORDER BY r.posted_at,r.id"), &[&tenant, &company, &order, &order_line]).await?;
    let mut groups: HashMap<i64, OrderLineReceipts> = HashMap::new();
    for row in &rows {
        let line = line(row)?;
        let kind = kind(row.try_get(12)?)?;
        let summary = groups
            .entry(line.order_line_id)
            .or_insert_with(|| OrderLineReceipts::empty(line.order_line_id));
        summary.order_id = row.try_get(16)?;
        summary.order_number = row.try_get(17)?;
        summary.order_line_number = Some(line.order_line_number);
        summary.description = Some(line.description);
        summary.unit = Some(line.unit);
        summary.ordered_quantity = Some(line.ordered_quantity);
        let sign = if kind == ReceiptKind::Receipt {
            Decimal::ONE
        } else {
            -Decimal::ONE
        };
        summary.received_quantity += sign * line.received_quantity;
        summary.rejected_quantity += sign * line.rejected_quantity;
        summary.receipts.push(LineReceipt {
            receipt_id: row.try_get(10)?,
            receipt_number: row.try_get(11)?,
            kind,
            reverses_receipt_id: row.try_get(13)?,
            received_on: row.try_get(14)?,
            posted_at: row.try_get(15)?,
            received_quantity: line.received_quantity,
            rejected_quantity: line.rejected_quantity,
            rejection_reason: line.rejection_reason,
        });
    }
    let mut summaries: Vec<_> = groups.into_values().collect();
    summaries.sort_by_key(|s| (s.order_number, s.order_line_number, s.order_line_id));
    Ok(summaries)
}

pub(crate) fn header_json(r: &Receipt) -> Value {
    json!({"id":r.id,"receipt_number":r.receipt_number,"kind":r.kind.as_str(),"status":r.status.as_str(),"order_id":r.order_id,"order_number":r.order_number,"supplier_id":r.supplier_id,"reverses_receipt_id":r.reverses_receipt_id,"reason":r.reason,"received_on":r.received_on,"supplier_reference":r.supplier_reference,"notes":r.notes,"created_by_user_id":r.created_by_user_id,"created_by_impersonator_id":r.created_by_impersonator_id,"posted_by_user_id":r.posted_by_user_id,"posted_by_impersonator_id":r.posted_by_impersonator_id,"posted_at":r.posted_at,"cancelled_at":r.cancelled_at})
}
pub(crate) fn line_json(l: &ReceiptLine) -> Value {
    json!({"id":l.id,"line_number":l.line_number,"order_line_id":l.order_line_id,"order_line_number":l.order_line_number,"description":l.description,"unit":l.unit,"ordered_quantity":l.ordered_quantity.to_string(),"received_quantity":l.received_quantity.to_string(),"rejected_quantity":l.rejected_quantity.to_string(),"rejection_reason":l.rejection_reason})
}
pub(crate) fn change(kind: &str, id: i64, old: Value, new: Value) -> Change {
    Change {
        kind: format!("supply_chain.receiving.{kind}"),
        id,
        old,
        new,
    }
}

pub(crate) async fn insert_event(
    sql: &Transaction<'_>,
    scope: &Scope,
    company: i64,
    receipt: &Receipt,
    from: Option<ReceiptStatus>,
    reason: Option<&str>,
) -> Result<Change> {
    let row = sql.query_one("INSERT INTO supply_chain_receiving_receipt_events(tenant_id,company_id,receipt_id,from_status,to_status,actor_user_id,actor_impersonator_id,reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id,from_status,to_status,actor_user_id,actor_impersonator_id,reason,occurred_at", &[&scope.tenant(), &company, &receipt.id, &from.map(ReceiptStatus::as_str), &receipt.status.as_str(), &scope.user(), &scope.impersonator().map(|i| i.user()), &reason]).await?;
    let e = event(&row)?;
    Ok(change(
        "receipt_event",
        e.id,
        json!({}),
        json!({"id":e.id,"receipt_id":receipt.id,"from_status":e.from_status.map(ReceiptStatus::as_str),"to_status":e.to_status.as_str(),"actor_user_id":e.actor_user_id,"actor_impersonator_id":e.actor_impersonator_id,"reason":e.reason,"occurred_at":e.occurred_at}),
    ))
}

pub(crate) async fn insert_lines(
    sql: &Transaction<'_>,
    scope: &Scope,
    company: i64,
    receipt: i64,
    lines: &[ReceiptLine],
    changes: &mut Vec<Change>,
) -> Result<()> {
    for l in lines {
        let row = sql.query_one(&format!("INSERT INTO supply_chain_receiving_receipt_lines AS l(tenant_id,company_id,receipt_id,line_number,order_line_id,order_line_number,description,unit,ordered_quantity,received_quantity,rejected_quantity,rejection_reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING {LINE}"), &[&scope.tenant(), &company, &receipt, &l.line_number, &l.order_line_id, &l.order_line_number, &l.description, &l.unit, &l.ordered_quantity, &l.received_quantity, &l.rejected_quantity, &l.rejection_reason]).await?;
        let saved = line(&row)?;
        let mut value = line_json(&saved);
        value["receipt_id"] = json!(receipt);
        changes.push(change("receipt_line", saved.id, json!({}), value));
    }
    Ok(())
}

pub(crate) async fn replace_lines(
    sql: &Transaction<'_>,
    scope: &Scope,
    company: i64,
    receipt: &Receipt,
    lines: &[ReceiptLine],
    changes: &mut Vec<Change>,
) -> Result<()> {
    sql.execute("DELETE FROM supply_chain_receiving_receipt_lines WHERE tenant_id=$1 AND company_id=$2 AND receipt_id=$3", &[&scope.tenant(), &company, &receipt.id]).await?;
    for l in &receipt.lines {
        changes.push(change("receipt_line", l.id, line_json(l), json!({})));
    }
    insert_lines(sql, scope, company, receipt.id, lines, changes).await
}

/// Posting refresh preserves receipt line identity; only order snapshots change.
pub(crate) async fn refresh_lines(
    sql: &Transaction<'_>,
    scope: &Scope,
    company: i64,
    receipt: &Receipt,
    lines: &[ReceiptLine],
    changes: &mut Vec<Change>,
) -> Result<()> {
    for (old, new) in receipt.lines.iter().zip(lines) {
        sql.execute("UPDATE supply_chain_receiving_receipt_lines SET order_line_number=$4,description=$5,unit=$6,ordered_quantity=$7 WHERE tenant_id=$1 AND company_id=$2 AND id=$3", &[&scope.tenant(), &company, &old.id, &new.order_line_number, &new.description, &new.unit, &new.ordered_quantity]).await?;
        changes.push(change(
            "receipt_line",
            old.id,
            line_json(old),
            line_json(new),
        ));
    }
    Ok(())
}

pub(crate) async fn transition(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company: i64,
    current: Receipt,
    to: ReceiptStatus,
    reason: Option<String>,
) -> Result<Receipt> {
    let scope = scope.clone();
    let id = current.id;
    tx.mutate(move |sql| Box::pin(async move {
        let row = if to == ReceiptStatus::Posted {
            sql.query_one(&format!("UPDATE supply_chain_receiving_receipts AS r SET status='posted',posted_by_user_id=$4,posted_by_impersonator_id=$5,posted_at=clock_timestamp() AT TIME ZONE 'UTC',updated_at=now() AT TIME ZONE 'UTC' WHERE tenant_id=$1 AND company_id=$2 AND id=$3 RETURNING {HEADER}"), &[&scope.tenant(), &company, &id, &scope.user(), &scope.impersonator().map(|i| i.user())]).await?
        } else {
            sql.query_one(&format!("UPDATE supply_chain_receiving_receipts AS r SET status='cancelled',cancelled_at=clock_timestamp() AT TIME ZONE 'UTC',updated_at=now() AT TIME ZONE 'UTC' WHERE tenant_id=$1 AND company_id=$2 AND id=$3 RETURNING {HEADER}"), &[&scope.tenant(), &company, &id]).await?
        };
        let updated = header(&row)?;
        let changes = vec![change("receipt", id, header_json(&current), header_json(&updated)), insert_event(sql, &scope, company, &updated, Some(current.status), reason.as_deref()).await?];
        Ok(((), changes))
    })).await?;
    get(tx.sql(), tx.context().tenant, company, id, true)
        .await?
        .ok_or(Error::Internal)
}
