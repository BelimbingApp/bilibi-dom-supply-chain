//! Durable facts around Bilimbi's order lifecycle and amendments. Publication
//! runs after the complete facade operation, never inside a low-level SQL
//! transition: receipt reversal changes lines after reopening the header.
use crate::{Order, OrderStatus, Outcome};
use bilibi_database::{Error, MutationTransaction, Result};
use bilibi_events::{Events, NewEvent};
use rust_decimal::Decimal;
use serde_json::{json, Value};

pub(crate) const APPROVED: &str = "supply_chain/purchasing.order_approved";
pub(crate) const AMENDED: &str = "supply_chain/purchasing.order_amended";
pub(crate) const STATUS_CHANGED: &str = "supply_chain/purchasing.order_status_changed";

/// Read under the facade's company lock, including on caller-owned writes.
pub(crate) async fn before(
    tx: &MutationTransaction<'_>,
    company: i64,
    order_id: i64,
) -> Result<Option<(OrderStatus, i32)>> {
    tx.sql()
        .query_opt(
            "SELECT status,amendment_count FROM supply_chain_purchasing_orders \
             WHERE tenant_id=$1 AND company_id=$2 AND id=$3 FOR UPDATE",
            &[&tx.context().tenant, &company, &order_id],
        )
        .await?
        .map(|row| {
            Ok((
                OrderStatus::parse(row.try_get(0)?).ok_or(Error::Internal)?,
                row.try_get(1)?,
            ))
        })
        .transpose()
}

pub(crate) async fn publish(
    events: &Events,
    tx: &mut MutationTransaction<'_>,
    company: i64,
    before: Option<(OrderStatus, i32)>,
    outcome: &Outcome<Order>,
) -> Result<()> {
    let (Some(before), Outcome::Saved(order)) = (before, outcome) else {
        return Ok(());
    };
    let result = snapshot(events, tx, company, before, order).await;
    if let Err(error) = result {
        // Events may refuse during preflight, before mutate poisons the tx.
        // Even a caller that catches the failure must not commit the order.
        let _ = tx
            .mutate::<(), _>(|_| Box::pin(async { Err(Error::Internal) }))
            .await;
        return Err(error);
    }
    Ok(())
}

async fn snapshot(
    events: &Events,
    tx: &mut MutationTransaction<'_>,
    company: i64,
    (from, amendment_count): (OrderStatus, i32),
    order: &Order,
) -> Result<()> {
    let changed = from != order.status;
    let amended = amendment_count != order.amendment_count;
    if !changed && !amended {
        return Ok(());
    }
    let history = if changed {
        let event = order.events.last().ok_or(Error::Internal)?;
        if event.from_status != Some(from) || event.to_status != order.status {
            return Err(Error::Internal);
        }
        Some(event.id)
    } else {
        None
    };
    let amendment = if amended {
        let amendment = order.amendments.last().ok_or(Error::Internal)?;
        if amendment.amendment_number != order.amendment_count {
            return Err(Error::Internal);
        }
        Some(amendment.id)
    } else {
        None
    };
    // Retain every generation, not just the current lines. Payables can still
    // match an old line ID and see both supersession and terminal order status.
    let rows = tx
        .sql()
        .query(
            "SELECT id,quantity,unit_price,superseded_by_amendment_id,unit,received_quantity \
         FROM supply_chain_purchasing_order_lines \
         WHERE tenant_id=$1 AND company_id=$2 AND order_id=$3 ORDER BY id",
            &[&tx.context().tenant, &company, &order.id],
        )
        .await?;
    let mut lines = Vec::with_capacity(rows.len());
    for row in rows {
        lines.push(json!({
            "id": row.try_get::<_, i64>(0)?,
            "quantity": row.try_get::<_, Decimal>(1)?.to_string(),
            "unit_price": row.try_get::<_, Decimal>(2)?.to_string(),
            "superseded_by_amendment_id": row.try_get::<_, Option<i64>>(3)?,
            "unit": row.try_get::<_, String>(4)?,
            "received_quantity": row.try_get::<_, Decimal>(5)?.to_string(),
            "currency_code": order.currency_code,
            "order_status": order.status.as_str(),
        }));
    }
    // One line per part avoids the queue's 100-member / 64-KiB bounds. An
    // empty draft can still be cancelled, and needs one empty status fact.
    if lines.is_empty() {
        lines.push(Value::Null);
    }
    let mut publications = Vec::new();
    if let Some(id) = history {
        publications.push((STATUS_CHANGED, format!("status:{id}")));
        if order.status == OrderStatus::Approved {
            publications.push((APPROVED, format!("approved:{id}")));
        }
    }
    if let Some(id) = amendment {
        publications.push((AMENDED, format!("amendment:{id}")));
    }
    let subject = format!("order:{}", order.id);
    for (name, snapshot_key) in publications {
        for (index, line) in lines.iter().enumerate() {
            let line_id = line["id"].as_i64();
            let key = format!("{snapshot_key}:line:{}", line_id.unwrap_or(0));
            events.record(tx, NewEvent {
                name, subject: &subject, key: &key,
                payload: json!({
                    "company_id": company, "order_id": order.id,
                    "order_number": order.order_number, "supplier_id": order.supplier_id,
                    "currency_code": order.currency_code, "order_status": order.status.as_str(),
                    "from_status": from.as_str(), "amendment_count": order.amendment_count,
                    "order_event_id": history, "amendment_id": amendment,
                    "snapshot_key": snapshot_key, "part_index": index + 1, "part_count": lines.len(),
                    "lines": if line_id.is_some() { vec![line] } else { vec![] },
                }),
            }).await?;
        }
    }
    Ok(())
}
