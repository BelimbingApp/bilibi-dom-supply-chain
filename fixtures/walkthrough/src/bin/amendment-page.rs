//! Prepare amendment browser proof after `./bilibi start` seeded an empty,
//! disposable database. Never install walkthrough users into a real database.
use bilibi_agent_api::{
    plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
    Registry,
};
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Error};
use bilibi_module_registry::Services;
use bilibi_session::Sessions;
use bilibi_supply_chain_purchasing::{
    NewOrder, NewOrderLine, NewSupplier, NewTaxCode, Outcome, Purchasing, SERVICE,
};
use bilibi_tenancy::{ProofFuture, Scope};
use bilibi_user::{AccountInput, UserService};
use serde_json::json;
use std::sync::Arc;
use tokio_postgres::Transaction;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Browser<'a> {
    sessions: &'a Sessions,
    token: &'a str,
}
impl PlanAuthority for Browser<'_> {
    fn verify<'a>(&'a self, tx: &'a Transaction<'_>) -> ProofFuture<'a, PlanIdentity> {
        Box::pin(async move {
            Ok(PlanIdentity {
                scope: self.sessions.hold(tx, self.token).await?,
                credential_binding: format!("amendment-proof:{}", self.token),
            })
        })
    }
}
fn saved<T>(outcome: Outcome<T>) -> Result<T> {
    match outcome {
        Outcome::Saved(record) => Ok(record),
        Outcome::Refused(reason) => Err(reason.text().into()),
        _ => Err("company archived".into()),
    }
}
async fn approved(
    p: &Purchasing,
    author: &Scope,
    approver: &Scope,
    company: i64,
    supplier: i64,
    tax: i64,
) -> Result<bilibi_supply_chain_purchasing::Order> {
    let order = saved(
        p.create_order(
            author,
            company,
            &NewOrder {
                supplier_id: Some(supplier),
                currency_code: "MYR".into(),
                reference: "Amendment proof".into(),
                notes: "Original notes".into(),
                expected_on: "2026-12-01".into(),
                lines: (1..=6)
                    .map(|index| NewOrderLine {
                        description: format!("Proof line {index}"),
                        external_item_ref: format!("ITEM-{index}"),
                        quantity: "2.5".into(),
                        unit: "pcs".into(),
                        unit_price: "3.2".into(),
                        tax_code_id: Some(tax),
                    })
                    .collect(),
            },
        )
        .await?,
    )?;
    saved(p.submit_order(author, company, order.id).await?)?;
    saved(
        p.approve_order(approver, company, order.id, "Within budget")
            .await?,
    )
}
#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("DATABASE_URL")?;
    // This executable adds accounts and records: require an explicit disposable-DB opt-in.
    if std::env::var("BILIBI_AMENDMENT_PROOF").as_deref() != Ok("disposable") {
        return Err("Set BILIBI_AMENDMENT_PROOF=disposable for the task database".into());
    }
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&url)?;
    let modules = bilibi_module_registry::discover()?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        Arc::new(bilibi_module_registry::Composition {
            version: env!("CARGO_PKG_VERSION"),
            commit: None,
            tag: None,
            pin: bilibi_module_registry::PinState::Unpinned,
            containers: vec![bilibi_module_registry::Container {
                layer: bilibi_module_registry::Layer::Domains,
                id: "supply_chain",
                modules: bilibi_domains::MODULES.iter().map(|(id, _)| *id).collect(),
                commit: None,
                version: None,
            }],
        }),
    )?;
    services.install(&modules)?;
    services
        .get::<bilibi_base::step_up::StepUp>(bilibi_base::step_up::SERVICE)?
        .set_enforced(false);
    let users = services.get::<UserService>("core/user.accounts")?;
    let sessions = services.get::<Sessions>("base/session.sessions")?;
    let login = users.login("ai@agent.my", "bilibi-dev").await?;
    let admin = sessions.check(&**db.get().await?, &login.token).await?;
    let company = admin
        .current_company()
        .ok_or("administrator needs a company")?
        .get();
    let approver = users
        .create_account(
            &admin,
            &AccountInput {
                name: "Amendment approver".into(),
                email: "amendment-approver@example.test".into(),
                password: "bilibi-dev".into(),
                ..AccountInput::default()
            },
        )
        .await?;
    let role: i64 = db
        .get()
        .await?
        .query_one(
            "SELECT id FROM roles WHERE tenant_id=$1 AND code='core_admin'",
            &[&admin.tenant()],
        )
        .await?
        .get(0);
    services
        .get::<Grants>("base/authz.grants")?
        .assign_role(&admin, approver.id, role, true)
        .await?;
    let second = users
        .login("amendment-approver@example.test", "bilibi-dev")
        .await?;
    let approver = sessions.check(&**db.get().await?, &second.token).await?;
    let p = services.get::<Purchasing>(SERVICE)?;
    let supplier = saved(
        p.create_supplier(
            &admin,
            company,
            &NewSupplier {
                code: "AMEND-PROOF".into(),
                name: "Amendment proof supplier".into(),
                ..NewSupplier::default()
            },
        )
        .await?,
    )?;
    let tax = saved(
        p.create_tax_code(
            &admin,
            company,
            &NewTaxCode {
                code: "AMEND-TAX".into(),
                name: "Proof tax".into(),
                rate: "10".into(),
                ..NewTaxCode::default()
            },
        )
        .await?,
    )?;
    let browser_order = approved(&p, &admin, &approver, company, supplier.id, tax.id).await?;
    println!(
        "Browser order: /supply-chain/purchase-orders/{}; lines={} status={:?}",
        browser_order.id,
        browser_order.lines.len(),
        browser_order.status
    );
    let agent_order = approved(&p, &admin, &approver, company, supplier.id, tax.id).await?;
    let key = "supply_chain.purchasing.order.amend";
    let op = PlannedOperation {
        key: key.into(),
        input: json!({"company_id":company,"order_id":agent_order.id,"expected_amendment_count":0,"reason":"Agent header proof","notes":"Agent reviewed notes","lines":agent_order.lines.iter().map(|line| json!({"description":line.description,"external_item_ref":line.external_item_ref,"quantity":line.quantity.to_string(),"unit":line.unit,"unit_price":line.unit_price.to_string(),"tax_code_id":line.tax_code_id})).collect::<Vec<_>>()}),
    };
    let registry = services.get::<Registry>("base/agent_api.registry")?;
    if registry
        .call(&**db.get().await?, &admin, key, &op.input)
        .await
        .is_ok()
    {
        return Err("direct agent write unexpectedly succeeded".into());
    }
    println!("Agent direct write refused");
    let plans = services.get::<Plans>("base/agent_api.plans")?;
    let authority = Browser {
        sessions: &sessions,
        token: &login.token,
    };
    let ops = [op];
    let preview = plans.preview(&authority, &ops).await?;
    let code = preview["plan_code"]
        .as_str()
        .ok_or("agent preview refused")?;
    let before = p
        .get_order(&admin, company, agent_order.id)
        .await?
        .ok_or(Error::NotFound)?;
    if before.amendment_count != 0 {
        return Err("preview wrote an amendment".into());
    }
    let result = plans.commit(&admin, &authority, code, &ops).await?;
    println!("Agent commit results={}", result["results"]);
    if plans.commit(&admin, &authority, code, &ops).await.is_ok() {
        return Err("agent replay unexpectedly succeeded".into());
    }
    let after = p
        .get_order(&admin, company, agent_order.id)
        .await?
        .ok_or(Error::NotFound)?;
    if after.amendment_count != 1
        || after.notes.as_deref() != Some("Agent reviewed notes")
        || after.lines.len() != 6
    {
        return Err("agent amendment result mismatch".into());
    }
    println!("Agent preview wrote nothing; commit retained six lines and recorded one amendment; replay refused");
    Ok(())
}
