//! H7 running-service specimen on disposable walkthrough data. Real sessions,
//! registry, plans, facades and audit; never prints credentials or plan codes.
use bilibi_agent_api::{
    plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
    Registry,
};
use bilibi_authz::grants::Grants;
use bilibi_database::Db;
use bilibi_module_registry::Services;
use bilibi_session::Sessions;
use bilibi_supply_chain_purchasing::{
    NewOrder, NewOrderLine, Outcome, Purchasing, TaxCodeChanges, MANAGE_ORDERS, VIEW,
};
use bilibi_tenancy::{ProofFuture, Scope};
use serde_json::{json, Value};
use tokio_postgres::Transaction;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const KEY: &str = "supply_chain.purchasing.order.amend";

struct Browser<'a> {
    sessions: &'a Sessions,
    token: &'a str,
}
impl PlanAuthority for Browser<'_> {
    fn verify<'a>(&'a self, tx: &'a Transaction<'_>) -> ProofFuture<'a, PlanIdentity> {
        Box::pin(async move {
            tx.query_opt(
                "SELECT id FROM sessions WHERE id=$1 AND expires_at>clock_timestamp() FOR UPDATE",
                &[&self.token],
            )
            .await?
            .ok_or(bilibi_database::Error::Unauthorized)?;
            Ok(PlanIdentity {
                scope: self.sessions.check(tx, self.token).await?,
                credential_binding: format!("browser:{}", self.token),
            })
        })
    }
}
fn saved<T>(outcome: Outcome<T>) -> Result<T> {
    match outcome {
        Outcome::Saved(record) => Ok(record),
        other => Err(format!(
            "expected saved record, got {}",
            match other {
                Outcome::Refused(reason) => reason.text(),
                _ => "company archived".into(),
            }
        )
        .into()),
    }
}
fn code(plan: &Value) -> Result<&str> {
    plan["plan_code"]
        .as_str()
        .ok_or("preview did not issue a code".into())
}
async fn preview(
    plans: &Plans,
    browser: &Browser<'_>,
    operations: &[PlannedOperation],
) -> Result<Value> {
    let mut plan = plans.preview(browser, operations).await?;
    let code = plan["plan_code"].take();
    println!("H7 review: {plan}; code issued={}", code.is_string());
    plan["plan_code"] = code;
    Ok(plan)
}
async fn state(db: &Db, id: i64) -> Result<()> {
    let row = db.get().await?.query_one(
        "SELECT status,amendment_count,notes,(SELECT count(*) FROM supply_chain_purchasing_order_lines WHERE order_id=o.id AND superseded_by_amendment_id IS NULL),(SELECT count(*) FROM supply_chain_purchasing_order_amendments WHERE order_id=o.id),(SELECT count(*) FROM base_audit_mutations WHERE auditable_type='supply_chain.purchasing.order_amendment' AND tenant_id=1) FROM supply_chain_purchasing_orders o WHERE tenant_id=1 AND company_id=1 AND id=$1", &[&id],
    ).await?;
    println!("H7 state: status={} count={} notes={:?} current_lines={} amendments={} tenant_amendment_audits={}", row.get::<_,String>(0), row.get::<_,i32>(1), row.get::<_,Option<String>>(2), row.get::<_,i64>(3), row.get::<_,i64>(4), row.get::<_,i64>(5));
    Ok(())
}

pub(super) async fn run(
    db: &Db,
    services: &Services,
    purchasing: &Purchasing,
    admin: &Scope,
) -> Result<()> {
    let accounts = services.get::<bilibi_user::UserService>("core/user.accounts")?;
    let sessions = services.get::<Sessions>("base/session.sessions")?;
    let plans = services.get::<Plans>("base/agent_api.plans")?;
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    let login = accounts
        .login("manager@example.test", bilibi_walkthrough_fixture::PASSWORD)
        .await?;
    let browser = Browser {
        sessions: &sessions,
        token: &login.token,
    };
    let manager = sessions.check(&**db.get().await?, &login.token).await?;
    let supplier = purchasing
        .list_suppliers(&manager, 1, true)
        .await?
        .remove(0);
    let tax = purchasing
        .list_tax_codes(&manager, 1, true)
        .await?
        .into_iter()
        .find(|tax| tax.code == "SST-10")
        .ok_or("missing tax code")?;
    let draft = saved(
        purchasing
            .create_order(
                &manager,
                1,
                &NewOrder {
                    supplier_id: Some(supplier.id),
                    currency_code: "MYR".into(),
                    reference: "H7".into(),
                    expected_on: "2026-12-01".into(),
                    notes: "Keep this unless supplied".into(),
                    lines: vec![NewOrderLine {
                        description: "Bracket".into(),
                        quantity: "2.5".into(),
                        unit: "pcs".into(),
                        unit_price: "3.2".into(),
                        tax_code_id: Some(tax.id),
                        ..NewOrderLine::default()
                    }],
                },
            )
            .await?,
    )?;
    saved(purchasing.submit_order(&manager, 1, draft.id).await?)?;
    saved(
        purchasing
            .approve_order(admin, 1, draft.id, "Within budget")
            .await?,
    )?;
    let operation = |count, quantity: &str, notes: &str| PlannedOperation {
        key: KEY.into(),
        input: json!({"company_id":1,"order_id":draft.id,"expected_amendment_count":count,"reason":" Delivery arrangements ","notes":notes,
            "lines":[{"description":" Bracket ","quantity":quantity,"unit":"pcs","unit_price":"3.2000","tax_code_id":tax.id}]}),
    };
    grants
        .set_direct(admin, manager.user(), VIEW, Some(false))
        .await?;
    println!(
        "H7 manage without view: {:?}",
        purchasing.get_order(&manager, 1, draft.id).await.err()
    );
    let ops = [operation(0, "2.5000", " Bay 3 ")];
    println!(
        "H7 direct write: {:?}",
        registry
            .call(&**db.get().await?, &manager, KEY, &ops[0].input)
            .await
    );
    state(db, draft.id).await?;
    let plan = preview(&plans, &browser, &ops).await?;
    state(db, draft.id).await?;
    println!(
        "H7 header-only commit: {:?}",
        plans.commit(&manager, &browser, code(&plan)?, &ops).await?
    );
    println!(
        "H7 replay: {:?}",
        plans.commit(&manager, &browser, code(&plan)?, &ops).await
    );
    state(db, draft.id).await?;

    let pair = [operation(1, "2.5", "First"), operation(1, "2.5", "Second")];
    let plan = preview(&plans, &browser, &pair).await?;
    println!(
        "H7 duplicate-order atomic rollback: {:?}",
        plans.commit(&manager, &browser, code(&plan)?, &pair).await
    );
    state(db, draft.id).await?;

    let ops = [operation(1, "2.5", "New tax review")];
    let plan = preview(&plans, &browser, &ops).await?;
    saved(
        purchasing
            .update_tax_code(
                &manager,
                1,
                tax.id,
                &TaxCodeChanges {
                    rate: Some("9".into()),
                    ..TaxCodeChanges::default()
                },
            )
            .await?,
    )?;
    println!(
        "H7 stale tax review: {:?}",
        plans.commit(&manager, &browser, code(&plan)?, &ops).await
    );
    saved(
        purchasing
            .update_tax_code(
                &manager,
                1,
                tax.id,
                &TaxCodeChanges {
                    rate: Some(tax.rate.to_string()),
                    ..TaxCodeChanges::default()
                },
            )
            .await?,
    )?;

    let ops = [operation(1, "2.5", "Permission check")];
    let plan = preview(&plans, &browser, &ops).await?;
    grants
        .set_direct(admin, manager.user(), MANAGE_ORDERS, Some(false))
        .await?;
    println!(
        "H7 permission revoked after preview: {:?}",
        plans.commit(&manager, &browser, code(&plan)?, &ops).await
    );
    grants
        .set_direct(admin, manager.user(), MANAGE_ORDERS, None)
        .await?;
    for mut op in [
        operation(1, "0", "Invalid quantity"),
        operation(1, "1.23456", "Invalid scale"),
    ] {
        op.input["reason"] = json!("");
        preview(&plans, &browser, &[op]).await?;
    }
    let mut empty = operation(1, "2.5", "No lines");
    empty.input["lines"] = json!([]);
    preview(&plans, &browser, &[empty]).await?;

    let ops = [operation(1, "3.5", "Changed quantity")];
    let plan = preview(&plans, &browser, &ops).await?;
    println!(
        "H7 changed-line commit: {:?}",
        plans.commit(&manager, &browser, code(&plan)?, &ops).await?
    );
    println!(
        "H7 own approval: {:?}",
        purchasing.approve_order(&manager, 1, draft.id, "").await?
    );
    saved(
        purchasing
            .approve_order(admin, 1, draft.id, "Reapproved")
            .await?,
    )?;
    saved(purchasing.send_order(&manager, 1, draft.id).await?)?;
    let ops = [operation(2, "3.5000", "Sent order header")];
    let plan = preview(&plans, &browser, &ops).await?;
    println!(
        "H7 sent header-only commit: {:?}",
        plans.commit(&manager, &browser, code(&plan)?, &ops).await?
    );
    state(db, draft.id).await?;
    grants.set_direct(admin, manager.user(), VIEW, None).await?;
    accounts.logout(&login.token, &login.csrf).await?;
    println!("H7 walkthrough complete; credentials removed, disposable evidence retained.");
    Ok(())
}
