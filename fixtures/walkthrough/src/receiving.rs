//! Running-stack S5 walkthrough, not unit tests. Business records use public
//! facades; SQL inspects evidence and probes refusals in this disposable DB.
use super::{decimal, order_line, saved, show, supplier, COMPANY};
use bilibi_authz::grants::Grants;
use bilibi_database::{Db, Error, MutationBoundary, Result};
use bilibi_module_registry::Services;
use bilibi_supply_chain_purchasing::{self as buying, NewOrder, Order, OrderStatus, Purchasing};
use bilibi_supply_chain_receiving::{
    self as receiving, NewReceipt, NewReceiptLine, Outcome, PostingQuery, ReceiptChanges,
    ReceiptQuery, ReceiptStatus, Receiving, Reversal,
};
use bilibi_tenancy::{Authentication, Scope};
use rust_decimal::Decimal;
use serde_json::Value;

fn check(holds: bool, label: &str) -> Result<()> {
    if !holds {
        eprintln!("FAILED: {label}");
        return Err(Error::Internal);
    }
    println!("S5 OK: {label}");
    Ok(())
}
fn kept<T>(outcome: Outcome<T>) -> Result<T> {
    match outcome {
        Outcome::Saved(value) => Ok(value),
        Outcome::Refused(reason) => {
            eprintln!("unexpected refusal: {}", reason.text());
            Err(Error::Invalid)
        }
        Outcome::CompanyArchived => Err(Error::Invalid),
    }
}
fn refused<T>(outcome: Outcome<T>, label: &str) -> Result<()> {
    if let Outcome::Refused(reason) = outcome {
        println!("S5 refused: {label}: {}", reason.text());
        Ok(())
    } else {
        check(false, label)
    }
}
fn line(id: i64, received: &str, rejected: &str, reason: &str) -> NewReceiptLine {
    NewReceiptLine {
        order_line_id: id,
        received_quantity: received.into(),
        rejected_quantity: rejected.into(),
        rejection_reason: reason.into(),
    }
}
fn receipt(order: &Order, received: &str, rejected: &str) -> NewReceipt {
    NewReceipt {
        order_id: order.id,
        supplier_reference: " Delivery note ".into(),
        lines: vec![line(
            order.lines[0].id,
            received,
            rejected,
            if rejected == "0" { "" } else { "Damaged" },
        )],
        ..NewReceipt::default()
    }
}
async fn sent(
    p: &Purchasing,
    manager: &Scope,
    admin: &Scope,
    supplier: i64,
    count: usize,
) -> Result<Order> {
    let lines = (0..count)
        .map(|i| {
            order_line(
                &format!("Receipt item {}", i + 1),
                "10",
                "unit",
                "2.75",
                None,
            )
        })
        .collect();
    let order = saved(show(
        "S5 draft order",
        p.create_order(
            manager,
            COMPANY,
            &NewOrder {
                supplier_id: Some(supplier),
                currency_code: "MYR".into(),
                lines,
                ..NewOrder::default()
            },
        )
        .await,
    ))?;
    saved(show(
        "S5 submit order",
        p.submit_order(manager, COMPANY, order.id).await,
    ))?;
    saved(show(
        "S5 approve order",
        p.approve_order(admin, COMPANY, order.id, "").await,
    ))?;
    saved(show(
        "S5 send order",
        p.send_order(manager, COMPANY, order.id).await,
    ))
}
async fn quantity(p: &Purchasing, scope: &Scope, order: i64) -> Result<(OrderStatus, Decimal)> {
    let o = p
        .get_order(scope, COMPANY, order)
        .await?
        .ok_or(Error::Internal)?;
    Ok((o.status, o.lines[0].received_quantity))
}
/// Business/audit/outbox snapshot, independent of the facade's result.
async fn evidence(db: &Db) -> Result<Value> {
    let c = db.get().await?;
    Ok(c.query_one("SELECT jsonb_build_array((SELECT count(*) FROM supply_chain_receiving_receipts),(SELECT count(*) FROM supply_chain_receiving_receipt_lines),(SELECT count(*) FROM supply_chain_receiving_receipt_events),(SELECT count(*) FROM supply_chain_purchasing_order_events),(SELECT count(*) FROM base_audit_mutations),(SELECT count(*) FROM base_audit_actions),(SELECT count(*) FROM base_events))", &[]).await?.try_get(0)?)
}

pub(super) async fn run(
    services: &Services,
    db: &Db,
    manager: &Scope,
    admin: &Scope,
    viewer: &Scope,
) -> Result<()> {
    let r = services.get::<Receiving>(receiving::SERVICE)?;
    let p = services.get::<Purchasing>(buying::SERVICE)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let auth = services.get::<Authentication>("base/tenancy.authentication")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let s = saved(show(
        "S5 supplier",
        p.create_supplier(manager, COMPANY, &supplier("RECEIPTS", "Receipt supplier"))
            .await,
    ))?;
    let order = sent(&p, manager, admin, s.id, 1).await?;
    let setting = r.get_settings(manager, COMPANY).await?;
    check(
        setting.over_receipt_tolerance_percent == Decimal::ZERO && setting.occurred_at.is_none(),
        "unset tolerance is zero without setter/time",
    )?;
    check(
        matches!(
            r.list_receipts(viewer, COMPANY, ReceiptQuery::default())
                .await,
            Err(Error::Forbidden)
        ),
        "Receiving read requires its own grant",
    )?;
    refused(
        r.create_receipt(manager, COMPANY, &receipt(&order, "10.0001", "0"))
            .await?,
        "zero tolerance, one ten-thousandth over",
    )?;
    for invalid in ["-0.0001", "100.0001", "2.00001", "", "1_0", "1e-5"] {
        refused(
            r.configure(manager, COMPANY, invalid).await?,
            "invalid tolerance",
        )?;
    }
    for (accepted, rejected, reason) in [
        ("0", "0", ""),
        ("1", "0", "Unneeded"),
        ("0", "1", ""),
        ("1.00001", "0", ""),
        ("-1", "0", ""),
        ("10000000000000000", "0", ""),
        ("1_0", "0", ""),
        ("1e-5", "0", ""),
    ] {
        let mut input = receipt(&order, "1", "0");
        input.lines = vec![line(order.lines[0].id, accepted, rejected, reason)];
        refused(
            r.create_receipt(manager, COMPANY, &input).await?,
            "invalid receipt quantities/reason",
        )?;
    }
    let mut duplicate = receipt(&order, "1", "0");
    duplicate.lines.push(duplicate.lines[0].clone());
    refused(
        r.create_receipt(manager, COMPANY, &duplicate).await?,
        "duplicate order line",
    )?;
    let mut invalid_date = receipt(&order, "1", "0");
    invalid_date.received_on = Some("2026-02-29".into());
    refused(
        r.create_receipt(manager, COMPANY, &invalid_date).await?,
        "invalid calendar date",
    )?;
    let draft = kept(
        r.create_receipt(manager, COMPANY, &receipt(&order, "3.125e0", "225e-2"))
            .await?,
    )?;
    check(
        draft.receipt_number == 1
            && draft.lines[0].received_quantity == decimal("3.125")
            && draft.lines[0].rejected_quantity == decimal("2.25")
            && draft.events.len() == 1
            && draft.supplier_reference.as_deref() == Some("Delivery note"),
        "failed drafts consume no numbers; exact exponent quantities, trim and history",
    )?;
    let draft = kept(
        r.update_receipt(
            manager,
            COMPANY,
            draft.id,
            &ReceiptChanges {
                notes: Some(" Count checked ".into()),
                lines: Some(vec![line(order.lines[0].id, "4.125", "2.25", "Damaged")]),
                ..ReceiptChanges::default()
            },
        )
        .await?,
    )?;
    check(
        draft.notes.as_deref() == Some("Count checked")
            && draft.lines[0].received_quantity == decimal("4.125"),
        "draft updates replace lines and trim notes",
    )?;
    let posted = kept(r.post_receipt(manager, COMPANY, draft.id).await?)?;
    check(
        posted.status == ReceiptStatus::Posted
            && posted.events.len() == 2
            && quantity(&p, manager, order.id).await?
                == (OrderStatus::PartiallyReceived, decimal("4.125")),
        "mixed receipt posts with Purchasing in one commit",
    )?;
    let summary = r
        .order_line_receipts(manager, COMPANY, order.lines[0].id)
        .await?;
    check(
        summary.received_quantity == decimal("4.125")
            && summary.rejected_quantity == decimal("2.25")
            && summary.receipts.len() == 1,
        "summary includes accepted and rejected quantities independently",
    )?;
    let empty = r.order_line_receipts(manager, COMPANY, i64::MAX).await?;
    check(
        empty.order_id.is_none()
            && empty.received_quantity == Decimal::ZERO
            && empty.receipts.is_empty(),
        "unknown line has null facts and zero contributions",
    )?;
    let before = evidence(db).await?;
    refused(
        r.post_receipt(manager, COMPANY, draft.id).await?,
        "cannot post twice",
    )?;
    check(
        evidence(db).await? == before,
        "duplicate post leaves business, audit and outbox unchanged",
    )?;

    // A revocation takes effect on the same sealed scope (a stale page).
    grants
        .set_direct(admin, manager.user(), receiving::POST_RECEIPTS, Some(false))
        .await?;
    check(
        matches!(
            r.reverse_receipt(
                manager,
                COMPANY,
                posted.id,
                &Reversal {
                    reason: "Correction".into(),
                    ..Reversal::default()
                }
            )
            .await,
            Err(Error::Forbidden)
        ),
        "mid-session post grant revocation denies reversal",
    )?;
    grants
        .set_direct(admin, manager.user(), receiving::POST_RECEIPTS, None)
        .await?;
    let reversal = kept(
        r.reverse_receipt(
            manager,
            COMPANY,
            posted.id,
            &Reversal {
                reason: " Correction ".into(),
                ..Reversal::default()
            },
        )
        .await?,
    )?;
    let summary = r
        .order_line_receipts(manager, COMPANY, order.lines[0].id)
        .await?;
    check(
        summary.received_quantity == Decimal::ZERO
            && summary.rejected_quantity == Decimal::ZERO
            && summary.receipts.len() == 2
            && reversal.notes.is_none(),
        "reversal subtracts both quantities and does not copy original notes",
    )?;
    let original = r
        .get_receipt(manager, COMPANY, posted.id)
        .await?
        .ok_or(Error::Internal)?;
    check(
        original.status == ReceiptStatus::Posted
            && original.reversed_by_receipt_id == Some(reversal.id)
            && reversal.reverses_receipt_number == Some(posted.receipt_number),
        "original remains posted; reversal links resolve both ways",
    )?;
    refused(
        r.reverse_receipt(
            manager,
            COMPANY,
            posted.id,
            &Reversal {
                reason: "Again".into(),
                ..Reversal::default()
            },
        )
        .await?,
        "original reversed only once",
    )?;
    refused(
        r.reverse_receipt(
            manager,
            COMPANY,
            reversal.id,
            &Reversal {
                reason: "Again".into(),
                ..Reversal::default()
            },
        )
        .await?,
        "reversal is not reversible",
    )?;

    // Current setting is checked at post, not fixed when drafted.
    kept(r.configure(manager, COMPANY, "10").await?)?;
    let over = kept(
        r.create_receipt(manager, COMPANY, &receipt(&order, "11", "0"))
            .await?,
    )?;
    kept(r.configure(manager, COMPANY, "0").await?)?;
    let before = evidence(db).await?;
    refused(
        r.post_receipt(manager, COMPANY, over.id).await?,
        "lowering tolerance after drafting refuses posting",
    )?;
    check(
        evidence(db).await? == before && quantity(&p, manager, order.id).await?.1 == Decimal::ZERO,
        "tolerance refusal leaves order and receipt evidence unchanged",
    )?;
    kept(r.configure(manager, COMPANY, "10").await?)?;
    kept(r.post_receipt(manager, COMPANY, over.id).await?)?;
    check(
        quantity(&p, manager, order.id).await? == (OrderStatus::Closed, decimal("11")),
        "exact tolerance ceiling is accepted and closes order",
    )?;
    kept(
        r.reverse_receipt(
            manager,
            COMPANY,
            over.id,
            &Reversal {
                reason: "Return delivery".into(),
                ..Reversal::default()
            },
        )
        .await?,
    )?;
    check(
        quantity(&p, manager, order.id).await? == (OrderStatus::PartiallyReceived, Decimal::ZERO),
        "reversal reopens closed order before changing lines",
    )?;
    kept(r.configure(manager, COMPANY, "0").await?)?;

    // Two concurrent drafts may each fit, but only one may consume capacity.
    let a = kept(
        r.create_receipt(manager, COMPANY, &receipt(&order, "6", "0"))
            .await?,
    )?;
    let b = kept(
        r.create_receipt(manager, COMPANY, &receipt(&order, "6", "0"))
            .await?,
    )?;
    let (a_result, b_result) = tokio::join!(
        r.post_receipt(manager, COMPANY, a.id),
        r.post_receipt(manager, COMPANY, b.id)
    );
    let outcomes = [a_result?, b_result?];
    check(
        outcomes
            .iter()
            .filter(|o| matches!(o, Outcome::Saved(_)))
            .count()
            == 1
            && outcomes
                .iter()
                .filter(|o| matches!(o, Outcome::Refused(_)))
                .count()
                == 1
            && quantity(&p, manager, order.id).await?.1 == decimal("6"),
        "concurrent posts serialize; one wins and one refuses",
    )?;
    let losing = r
        .list_receipts(
            manager,
            COMPANY,
            ReceiptQuery {
                status: Some(ReceiptStatus::Draft),
                order_id: Some(order.id),
                ..ReceiptQuery::default()
            },
        )
        .await?;
    let cancelled = kept(
        r.cancel_receipt(manager, COMPANY, losing[0].id, "Duplicate delivery")
            .await?,
    )?;
    check(
        cancelled.status == ReceiptStatus::Cancelled
            && cancelled.events.last().and_then(|e| e.reason.as_deref())
                == Some("Duplicate delivery"),
        "cancellation retains number and reason history",
    )?;

    // Reject-only deliveries do not ask Purchasing for its receive grant.
    let rejects = kept(
        r.create_receipt(manager, COMPANY, &receipt(&order, "0", "300"))
            .await?,
    )?;
    grants
        .set_direct(admin, manager.user(), buying::RECEIVE_ORDERS, Some(false))
        .await?;
    let rejects = kept(r.post_receipt(manager, COMPANY, rejects.id).await?)?;
    grants
        .set_direct(admin, manager.user(), buying::VIEW, Some(false))
        .await?;
    kept(
        r.reverse_receipt(
            manager,
            COMPANY,
            rejects.id,
            &Reversal {
                reason: "Inspection correction".into(),
                ..Reversal::default()
            },
        )
        .await?,
    )?;
    grants
        .set_direct(admin, manager.user(), buying::VIEW, None)
        .await?;
    check(
        quantity(&p, manager, order.id).await?.1 == decimal("6"),
        "rejected-only post/reverse skips Purchasing write authority and reversal read authority",
    )?;
    let blocked = kept(
        r.create_receipt(manager, COMPANY, &receipt(&order, "1", "0"))
            .await?,
    )?;
    let before = evidence(db).await?;
    check(
        matches!(
            r.post_receipt(manager, COMPANY, blocked.id).await,
            Err(Error::Forbidden)
        ) && evidence(db).await? == before,
        "accepted receipt still requires Purchasing receive authority",
    )?;
    grants
        .set_direct(admin, manager.user(), buying::RECEIVE_ORDERS, None)
        .await?;

    // A real outbox SQL failure happens after Purchasing has changed.
    let c = db.get().await?;
    c.batch_execute("CREATE FUNCTION walkthrough_refuse_receipt_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.publisher='supply_chain/receiving' THEN RAISE EXCEPTION 'walkthrough outbox refusal'; END IF; RETURN NEW; END $$; CREATE TRIGGER walkthrough_refuse_receipt_event BEFORE INSERT ON base_events FOR EACH ROW EXECUTE FUNCTION walkthrough_refuse_receipt_event()").await?;
    let before = evidence(db).await?;
    let failed = r.post_receipt(manager, COMPANY, blocked.id).await;
    c.batch_execute("DROP TRIGGER walkthrough_refuse_receipt_event ON base_events; DROP FUNCTION walkthrough_refuse_receipt_event()").await?;
    check(
        failed.is_err()
            && evidence(db).await? == before
            && quantity(&p, manager, order.id).await?.1 == decimal("6")
            && r.get_receipt(manager, COMPANY, blocked.id)
                .await?
                .ok_or(Error::Internal)?
                .status
                == ReceiptStatus::Draft,
        "late outbox failure rolls back receipt, order, history and audit",
    )?;
    let blocked = kept(r.post_receipt(manager, COMPANY, blocked.id).await?)?;
    grants
        .set_direct(admin, manager.user(), buying::RECEIVE_ORDERS, Some(false))
        .await?;
    let before = evidence(db).await?;
    check(
        matches!(
            r.reverse_receipt(
                manager,
                COMPANY,
                blocked.id,
                &Reversal {
                    reason: "Permission rollback proof".into(),
                    ..Reversal::default()
                }
            )
            .await,
            Err(Error::Forbidden)
        ) && evidence(db).await? == before,
        "Purchasing refusal rolls back provisional reversal and its number",
    )?;
    grants
        .set_direct(admin, manager.user(), buying::RECEIVE_ORDERS, None)
        .await?;

    // Superseded lines refuse even when the delivery is entirely rejected.
    let amend_order = sent(&p, manager, admin, s.id, 1).await?;
    let stale = kept(
        r.create_receipt(manager, COMPANY, &receipt(&amend_order, "0", "1"))
            .await?,
    )?;
    saved(show(
        "S5 amend away line",
        p.amend_order(
            manager,
            COMPANY,
            amend_order.id,
            &buying::OrderAmendment {
                reason: "Replace item".into(),
                lines: vec![order_line("Replacement item", "10", "unit", "2.75", None)],
                ..buying::OrderAmendment::default()
            },
        )
        .await,
    ))?;
    saved(show(
        "S5 reapprove",
        p.approve_order(admin, COMPANY, amend_order.id, "").await,
    ))?;
    saved(show(
        "S5 resend",
        p.send_order(manager, COMPANY, amend_order.id).await,
    ))?;
    refused(
        r.post_receipt(manager, COMPANY, stale.id).await?,
        "superseded line cannot post",
    )?;
    grants
        .set_direct(admin, manager.user(), buying::VIEW, Some(false))
        .await?;
    kept(
        r.update_receipt(
            manager,
            COMPANY,
            stale.id,
            &ReceiptChanges {
                notes: Some("Header only".into()),
                ..ReceiptChanges::default()
            },
        )
        .await?,
    )?;
    kept(
        r.cancel_receipt(manager, COMPANY, stale.id, "Superseded")
            .await?,
    )?;
    grants
        .set_direct(admin, manager.user(), buying::VIEW, None)
        .await?;

    // Supplied transaction attribution cannot be spliced with another scope.
    let mut connection = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut connection,
            admin.mutation_context("walkthrough.mismatch"),
        )
        .await?;
    check(
        matches!(
            p.record_receipt_in(
                &mut tx,
                manager,
                COMPANY,
                order.id,
                &[buying::ReceiptLine {
                    line_id: order.lines[0].id,
                    quantity: Decimal::ONE
                }],
                Decimal::ZERO
            )
            .await,
            Err(Error::Forbidden)
        ),
        "transaction actor/scope mismatch refuses",
    )?;
    tx.rollback().await?;
    let impersonated = auth
        .authenticate_session(
            &**c,
            manager.tenant(),
            manager.user(),
            Some(admin.user()),
            "walkthrough".into(),
        )
        .await?;
    let mut tx = boundary
        .begin(
            &mut connection,
            manager.mutation_context("walkthrough.mismatch"),
        )
        .await?;
    check(
        matches!(
            p.reverse_receipt_in(
                &mut tx,
                &impersonated,
                COMPANY,
                order.id,
                &[buying::ReceiptLine {
                    line_id: order.lines[0].id,
                    quantity: Decimal::ONE
                }],
                "Correction"
            )
            .await,
            Err(Error::Forbidden)
        ),
        "transaction impersonator mismatch refuses",
    )?;
    tx.rollback().await?;
    let imp_draft = kept(
        r.create_receipt(&impersonated, COMPANY, &receipt(&order, "0", "1"))
            .await?,
    )?;
    let imp_posted = kept(r.post_receipt(&impersonated, COMPANY, imp_draft.id).await?)?;
    check(
        imp_posted.created_by_impersonator_id == Some(admin.user())
            && imp_posted.posted_by_impersonator_id == Some(admin.user())
            && imp_posted
                .events
                .iter()
                .all(|e| e.actor_impersonator_id == Some(admin.user())),
        "receipt and history preserve impersonation attribution",
    )?;

    check(
        matches!(
            r.get_receipt(manager, 100, posted.id).await,
            Err(Error::NotFound)
        ),
        "cross-tenant company remains hidden even with Receiving grants",
    )?;
    check(
        r.get_receipt(manager, 2, posted.id).await?.is_none(),
        "sibling company cannot read another company's receipt",
    )?;

    let page1 = r
        .list_postings(
            manager,
            COMPANY,
            PostingQuery {
                limit: 1,
                ..PostingQuery::default()
            },
        )
        .await?;
    let page2 = r
        .list_postings(
            manager,
            COMPANY,
            PostingQuery {
                after_posting_id: Some(page1[0].posting_id),
                limit: 1,
            },
        )
        .await?;
    check(
        page1[0].receipt.id == posted.id
            && page2[0].receipt.id == reversal.id
            && page2[0].posting_id > page1[0].posting_id
            && page1[0].receipt.events.is_empty()
            && page1[0].receipt.reversed_by_receipt_id == Some(reversal.id),
        "cursor is ascending history ID; views include later reversal links without history",
    )?;
    check(
        matches!(
            r.list_postings(
                manager,
                COMPANY,
                PostingQuery {
                    after_posting_id: Some(0),
                    limit: 1
                }
            )
            .await,
            Err(Error::Invalid)
        ),
        "explicit zero cursor refuses",
    )?;
    check(
        r.find_receipt_by_number(manager, COMPANY, posted.receipt_number)
            .await?
            .ok_or(Error::Internal)?
            .id
            == posted.id,
        "number lookup returns full receipt",
    )?;
    check(
        r.order_receipts(manager, COMPANY, order.id).await?[0].received_quantity == decimal("7"),
        "order summary agrees with Purchasing net accepted quantity",
    )?;

    // Database-level retention and deferred history, not just facade guards.
    for sql in [
        "UPDATE supply_chain_receiving_receipts SET notes='Rewrite' WHERE id=$1",
        "DELETE FROM supply_chain_receiving_receipts WHERE id=$1",
        "UPDATE supply_chain_receiving_receipt_lines SET description='Rewrite' WHERE receipt_id=$1",
        "DELETE FROM supply_chain_receiving_receipt_events WHERE receipt_id=$1",
    ] {
        check(
            c.execute(sql, &[&posted.id]).await.is_err(),
            "PostgreSQL refuses posted evidence rewrite/delete",
        )?;
    }
    check(
        c.execute(
            "UPDATE supply_chain_receiving_settings SET over_receipt_tolerance_percent=99",
            &[],
        )
        .await
        .is_err(),
        "PostgreSQL refuses settings rewrite",
    )?;
    check(
        c.batch_execute("TRUNCATE supply_chain_receiving_receipt_events")
            .await
            .is_err(),
        "PostgreSQL refuses history truncate",
    )?;
    let pending = kept(
        r.create_receipt(manager, COMPANY, &receipt(&order, "1", "0"))
            .await?,
    )?;
    check(c.execute("UPDATE supply_chain_receiving_receipts SET status='cancelled',cancelled_at=now() AT TIME ZONE 'UTC' WHERE id=$1", &[&pending.id]).await.is_err(), "deferred history trigger refuses unaudited transition at commit")?;

    // Large receipts exceed both a 100-line array and 64 KiB combined payload.
    let large = sent(&p, manager, admin, s.id, 101).await?;
    let large_input = NewReceipt {
        order_id: large.id,
        notes: "界".repeat(1000),
        lines: large
            .lines
            .iter()
            .map(|l| line(l.id, "1", "2", &"損".repeat(500)))
            .collect(),
        ..NewReceipt::default()
    };
    let large_draft = kept(r.create_receipt(manager, COMPANY, &large_input).await?)?;
    let large_posted = kept(r.post_receipt(manager, COMPANY, large_draft.id).await?)?;
    let row = c.query_one("SELECT count(*),sum(octet_length(payload::text)),max(octet_length(payload::text)),min((payload->>'part_count')::integer),max((payload->>'part_index')::integer) FROM base_events WHERE publisher='supply_chain/receiving' AND (payload->>'receipt_id')::bigint=$1", &[&large_posted.id]).await?;
    check(
        row.try_get::<_, i64>(0)? == 101
            && row.try_get::<_, i64>(1)? > 65536
            && row.try_get::<_, i32>(2)? < 65536
            && row.try_get::<_, i32>(3)? == 101
            && row.try_get::<_, i32>(4)? == 101,
        "101-line Unicode receipt publishes complete bounded parts without a new line limit",
    )?;
    let immutable: Value = c.query_one("SELECT payload FROM base_events WHERE publisher='supply_chain/receiving' AND (payload->>'receipt_id')::bigint=$1 ORDER BY id LIMIT 1", &[&posted.id]).await?.try_get(0)?;
    check(
        immutable["kind"] == "receipt"
            && immutable["line"]["received_quantity"]
                .as_str()
                .and_then(|s| s.parse::<Decimal>().ok())
                == Some(decimal("4.125"))
            && immutable.get("reversed_by_receipt_id").is_none(),
        "outbox retains immutable original facts after reversal",
    )?;
    check(
        blocked.lines[0].id != posted.lines[0].id,
        "distinct deliveries retain distinct receipt-line identities",
    )?;
    let percent = kept(r.configure(manager, COMPANY, "25e-1").await?)?;
    check(
        percent.over_receipt_tolerance_percent == decimal("2.5"),
        "exponent tolerance is parsed exactly",
    )?;
    let companies = services.get::<bilibi_company::CompanyService>("core/company.companies")?;
    companies
        .archive_company(admin, 62, Some("Receiving read-only proof"))
        .await
        .map_err(|_| Error::Internal)?;
    check(
        matches!(r.configure(admin, 62, "1").await?, Outcome::CompanyArchived)
            && r.get_settings(admin, 62).await?.occurred_at.is_none(),
        "archived company refuses writes but preserves reads",
    )?;
    println!("S5 Receiving walkthrough completed: lifecycle, tolerance, concurrency, permissions, atomic rollback, retention, summaries, cursor and outbox verified.");
    Ok(())
}
