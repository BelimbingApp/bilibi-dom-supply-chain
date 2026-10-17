//! S7 running-stack evidence. Only disposable fixtures; business writes use
//! the public facade, SQL observes the outbox and injects deliberate failures.
use super::{decimal, order_line, COMPANY};
use bilibi_database::{Db, Error, MutationBoundary, Result};
use bilibi_module_registry::Services;
use bilibi_supply_chain_purchasing::{
    NewOrder, OrderAmendment, OrderStatus, Outcome, Purchasing, ReceiptLine,
};
use bilibi_tenancy::Scope;
use serde_json::Value;

fn check(holds: bool, label: &str) -> Result<()> {
    if !holds {
        eprintln!("S7 FAILED: {label}");
        return Err(Error::Internal);
    }
    println!("S7 OK: {label}");
    Ok(())
}
fn kept<T>(outcome: Outcome<T>) -> Result<T> {
    match outcome {
        Outcome::Saved(record) => Ok(record),
        _ => Err(Error::Invalid),
    }
}
async fn facts(db: &Db, order: i64, suffix: &str) -> Result<Vec<Value>> {
    Ok(db.get().await?.query(
        "SELECT payload FROM base_events WHERE publisher='supply_chain/purchasing' AND subject=$1 AND name=$2 ORDER BY id",
        &[&format!("order:{order}"), &format!("supply_chain/purchasing.{suffix}")],
    ).await?.iter().map(|r| r.get(0)).collect())
}
async fn evidence(db: &Db, order: i64) -> Result<Value> {
    Ok(db.get().await?.query_one(
        "SELECT jsonb_build_array(to_jsonb(o), \
         (SELECT jsonb_agg(to_jsonb(l) ORDER BY id) FROM supply_chain_purchasing_order_lines l WHERE order_id=o.id), \
         (SELECT count(*) FROM supply_chain_purchasing_order_events), \
         (SELECT count(*) FROM supply_chain_purchasing_order_amendments), \
         (SELECT count(*) FROM base_audit_mutations), (SELECT count(*) FROM base_audit_actions), \
         (SELECT count(*) FROM base_events)) FROM supply_chain_purchasing_orders o WHERE id=$1",
        &[&order],
    ).await?.get(0))
}

pub(super) async fn run(
    services: &Services,
    db: &Db,
    manager: &Scope,
    admin: &Scope,
    viewer: &Scope,
) -> Result<()> {
    let p = services.get::<Purchasing>(bilibi_supply_chain_purchasing::SERVICE)?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let supplier = p.list_suppliers(manager, COMPANY, true).await?[0].id;
    let input = NewOrder {
        supplier_id: Some(supplier),
        currency_code: "MYR".into(),
        reference: "S7".into(),
        lines: vec![
            order_line("Order event item A", "3.2500", "box", "17.1250", None),
            order_line("Order event item B", "7.1250", "unit", "0.0625", None),
        ],
        ..NewOrder::default()
    };
    let draft = kept(p.create_order(manager, COMPANY, &input).await?)?;
    check(
        facts(db, draft.id, "order_status_changed")
            .await?
            .is_empty(),
        "draft creation publishes no transition",
    )?;
    kept(p.submit_order(manager, COMPANY, draft.id).await?)?;
    let before = evidence(db, draft.id).await?;
    check(
        matches!(
            p.approve_order(manager, COMPANY, draft.id, "").await?,
            Outcome::Refused(_)
        ),
        "submitter cannot approve",
    )?;
    check(
        matches!(
            p.approve_order(viewer, COMPANY, draft.id, "").await,
            Err(Error::Forbidden)
        ),
        "missing capability cannot approve",
    )?;
    check(
        before == evidence(db, draft.id).await?,
        "refusals create no order, history, audit or outbox change",
    )?;
    let approved = kept(
        p.approve_order(admin, COMPANY, draft.id, "S7 approval")
            .await?,
    )?;
    let approval = facts(db, draft.id, "order_approved").await?;
    check(
        approval.len() == 2
            && approval[0]["lines"][0]["id"] == approved.lines[0].id
            && decimal(approval[0]["lines"][0]["quantity"].as_str().unwrap_or(""))
                == decimal("3.25")
            && decimal(approval[0]["lines"][0]["unit_price"].as_str().unwrap_or(""))
                == decimal("17.125")
            && approval[1]["lines"][0]["id"] == approved.lines[1].id
            && approval[1]["lines"][0]["currency_code"] == "MYR",
        "approval emits two asymmetric exact-decimal line facts",
    )?;

    let amendment = OrderAmendment {
        reason: "S7 header correction".into(),
        notes: Some("Delivery instructions corrected".into()),
        lines: input.lines.clone(),
        ..OrderAmendment::default()
    };
    let unchanged = kept(
        p.amend_order(manager, COMPANY, draft.id, &amendment)
            .await?,
    )?;
    let header = facts(db, draft.id, "order_amended").await?;
    check(
        unchanged.status == OrderStatus::Approved
            && header.len() == 4
            && header.iter().all(|v| {
                v["order_event_id"].is_null()
                    && v["from_status"] == "approved"
                    && v["order_status"] == "approved"
            })
            && header[0]["lines"][0]["superseded_by_amendment_id"] == unchanged.amendments[0].id
            && header[2]["lines"][0]["superseded_by_amendment_id"].is_null()
            && facts(db, draft.id, "order_approved").await?.len() == 2,
        "header-only amendment supersedes identities without a false approval or status event",
    )?;
    let mut changed = amendment;
    changed.reason = "S7 quantity correction".into();
    changed.lines[0].quantity = "4.5001".into();
    let amended = kept(p.amend_order(manager, COMPANY, draft.id, &changed).await?)?;
    let changes = facts(db, draft.id, "order_amended").await?;
    check(
        amended.status == OrderStatus::Submitted
            && changes.len() == 10
            && changes[4..]
                .iter()
                .all(|v| v["order_status"] == "submitted" && v["part_count"] == 6)
            && changes[8]["lines"][0]["quantity"]
                .as_str()
                .is_some_and(|s| decimal(s) == decimal("4.5001")),
        "changed-line amendment resubmits and snapshots all three retained generations",
    )?;
    kept(
        p.approve_order(admin, COMPANY, draft.id, "S7 reapproval")
            .await?,
    )?;
    kept(p.send_order(manager, COMPANY, draft.id).await?)?;
    kept(
        p.close_order(manager, COMPANY, draft.id, "S7 closed short")
            .await?,
    )?;
    let statuses = facts(db, draft.id, "order_status_changed").await?;
    check(
        facts(db, draft.id, "order_approved").await?.len() == 8
            && statuses.len() == 28
            && statuses[22..].iter().all(|v| {
                v["order_status"] == "closed" && v["lines"][0]["order_status"] == "closed"
            }),
        "reapproval, send and close include superseded lines and final order status",
    )?;

    let empty = kept(
        p.create_order(
            manager,
            COMPANY,
            &NewOrder {
                lines: vec![],
                ..input.clone()
            },
        )
        .await?,
    )?;
    kept(
        p.cancel_order(manager, COMPANY, empty.id, "S7 empty draft")
            .await?,
    )?;
    let cancelled = facts(db, empty.id, "order_status_changed").await?;
    check(
        cancelled.len() == 1
            && cancelled[0]["lines"] == serde_json::json!([])
            && cancelled[0]["part_count"] == 1,
        "empty cancellation emits one empty part",
    )?;

    // Inject a later-part SQL failure, after part 1 was recorded successfully.
    let rollback = kept(p.create_order(manager, COMPANY, &input).await?)?;
    kept(p.submit_order(manager, COMPANY, rollback.id).await?)?;
    let before = evidence(db, rollback.id).await?;
    let c = db.get().await?;
    c.batch_execute("CREATE FUNCTION walkthrough_refuse_order_part() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.publisher='supply_chain/purchasing' AND NEW.payload->>'part_index'='2' THEN RAISE EXCEPTION 'S7 later-part refusal'; END IF; RETURN NEW; END $$; CREATE TRIGGER walkthrough_refuse_order_part BEFORE INSERT ON base_events FOR EACH ROW EXECUTE FUNCTION walkthrough_refuse_order_part()").await?;
    let failure = p
        .approve_order(admin, COMPANY, rollback.id, "S7 forced refusal")
        .await;
    c.batch_execute("DROP TRIGGER walkthrough_refuse_order_part ON base_events; DROP FUNCTION walkthrough_refuse_order_part()").await?;
    check(
        failure.is_err() && before == evidence(db, rollback.id).await?,
        "later-part failure rolls back approval, history, audit and earlier outbox parts",
    )?;

    // A caller-owned receipt closes this order. Reserve the next history ID
    // in this isolated DB to inject a conflicting key before publication.
    kept(
        p.approve_order(admin, COMPANY, rollback.id, "S7 recovered")
            .await?,
    )?;
    let sent = kept(p.send_order(manager, COMPANY, rollback.id).await?)?;
    let receipts: Vec<ReceiptLine> = sent
        .lines
        .iter()
        .map(|l| ReceiptLine {
            line_id: l.id,
            quantity: l.quantity,
        })
        .collect();
    let before = evidence(db, rollback.id).await?;
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut client,
            manager.mutation_context("supply_chain.walkthrough.order_conflict"),
        )
        .await?;
    let next: i64 = tx.sql().query_one("SELECT nextval(pg_get_serial_sequence('supply_chain_purchasing_order_events','id')) + 1", &[]).await?.get(0);
    // This deliberately bad fact is fixture fault injection only and rolls
    // back with the poisoned transaction, along with its audit row.
    services
        .get::<bilibi_events::Events>(bilibi_events::SERVICE)?
        .record(
            &mut tx,
            bilibi_events::NewEvent {
                name: "supply_chain/purchasing.order_status_changed",
                subject: &format!("order:{}", sent.id),
                key: &format!("status:{next}:line:{}", sent.lines[0].id),
                payload: serde_json::json!({}),
            },
        )
        .await?;
    let failure = p
        .record_receipt_in(&mut tx, manager, COMPANY, sent.id, &receipts, decimal("0"))
        .await;
    check(
        matches!(failure, Err(Error::Conflict)),
        "caller-owned receipt hits application-level outbox conflict",
    )?;
    check(
        tx.commit().await.is_err(),
        "catching preflight error cannot commit the caller-owned transaction",
    )?;
    check(
        before == evidence(db, rollback.id).await?,
        "preflight refusal rolls back quantities, closure, history, audit and outbox",
    )?;
    drop(client);
    kept(
        p.record_receipt(manager, COMPANY, sent.id, &receipts, decimal("0"))
            .await?,
    )?;
    let closed = facts(db, sent.id, "order_status_changed").await?;
    check(
        closed[closed.len() - 2..].iter().all(|v| {
            v["order_status"] == "closed"
                && v["lines"][0]["received_quantity"] == v["lines"][0]["quantity"]
        }),
        "full receipt publishes final quantities with auto-close",
    )?;
    kept(
        p.reverse_receipt(manager, COMPANY, sent.id, &receipts, "S7 reversal")
            .await?,
    )?;
    let reopened = facts(db, sent.id, "order_status_changed").await?;
    check(
        reopened[reopened.len() - 2..].iter().all(|v| {
            v["order_status"] == "partially_received"
                && v["lines"][0]["received_quantity"]
                    .as_str()
                    .is_some_and(|s| decimal(s).is_zero())
        }),
        "reopening publishes quantities after reversal, not before",
    )?;
    let count = reopened.len();
    kept(
        p.record_receipt(
            manager,
            COMPANY,
            sent.id,
            &[ReceiptLine {
                line_id: sent.lines[0].id,
                quantity: decimal("0.5"),
            }],
            decimal("0"),
        )
        .await?,
    )?;
    check(
        facts(db, sent.id, "order_status_changed").await?.len() == count,
        "unchanged-status receipt emits no status event",
    )?;

    let c = db.get().await?;
    let large: i64 = c.query_one("SELECT count(*) FROM base_events WHERE publisher='supply_chain/purchasing' AND (payload->>'part_count')::integer>100", &[]).await?.get(0);
    check(
        large > 100,
        "existing 101-line Receiving walkthrough also publishes complete bounded order parts",
    )?;
    for row in c.query("SELECT id,name,version,subject,event_key,payload FROM base_events WHERE publisher='supply_chain/purchasing' AND subject=$1 ORDER BY id", &[&format!("order:{}", draft.id)]).await? {
        println!("S7 OUTBOX {} {} v{} {} {} {}", row.get::<_,i64>(0), row.get::<_,String>(1), row.get::<_,i32>(2), row.get::<_,String>(3), row.get::<_,String>(4), row.get::<_,Value>(5));
    }
    println!(
        "S7 complete: approval, amendments, close and failure recovery; order {}",
        draft.id
    );
    Ok(())
}
