//! `DATABASE_URL=… cargo run -p bilibi-supply-chain-walkthrough`: the Supply
//! Chain walkthrough fixture for slice proofs. Against a freshly created
//! disposable database it migrates the composed app, installs the Platform
//! walkthrough fixture (its tenants, accounts and companies), gives the
//! manager the Tenant Owner role that carries every Purchasing capability,
//! and through the Purchasing facade creates Company 001's first suppliers
//! and tax codes, changes and deactivates them, walks purchase requests
//! through their lifecycle (draft, update, submit, approve, reject, cancel),
//! orders the approved request, prices, submits, returns, approves and sends
//! the order, records and reverses receipts until it closes, amends a second
//! order, and reads an order line through the
//! `supply_chain/purchasing.order_line` port. It shows the refusals beside
//! the writes and the database refusing a change to a submitted request, a
//! closed order, their lines or their history, and leaves two requests the
//! grant-all admin submitted for the manager to decide. A `bilibi-web run`
//! against the same database then shows the Supply chain menu and its
//! Suppliers and Purchase requests pages to `manager@example.test` and
//! withholds them from `viewer@example.test`.
//!
//! Never a product dependency, and never run against a real install: the
//! Platform fixture refuses a database that already holds other accounts.
mod agent;
mod order_events;
mod receiving;

use bilibi_authz::{
    grants::Grants,
    roles::{self, Query, Sort},
};
use bilibi_database::{pool, Error, Result};
use bilibi_module_registry::{verify_composition, Services};
use bilibi_ports::{PortError, Ports};
use bilibi_supply_chain_purchasing::{
    NewOrder, NewOrderLine, NewRequest, NewRequestLine, NewSupplier, NewTaxCode, Order,
    OrderAmendment, OrderChanges, OrderFromRequest, OrderQuery, Outcome, Purchasing, ReceiptLine,
    Request, RequestChanges, RequestQuery, Supplier, SupplierChanges, TaxCode, TaxCodeChanges,
    SERVICE,
};
use bilibi_tenancy::{Authentication, Scope};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::{fmt::Debug, sync::Arc};

/// Tenant 1 (the platform operator), its primary company and the fixture's
/// manager (affiliated with that company), viewer and grant-all admin.
const TENANT: i64 = 1;
const COMPANY: i64 = 1;
const OTHER_COMPANY: i64 = 2;
const MANAGER: i64 = 1;
const VIEWER: i64 = 2;
const ADMIN: i64 = 4;

fn supplier(code: &str, name: &str) -> NewSupplier {
    NewSupplier {
        code: code.into(),
        name: name.into(),
        ..NewSupplier::default()
    }
}

fn suppliers() -> [NewSupplier; 2] {
    [
        NewSupplier {
            contact_name: "Farah Lim".into(),
            email: "farah@kedai-besi.example.test".into(),
            phone: "+60 3 2000 1000".into(),
            address_line1: "12 Jalan Perusahaan".into(),
            locality: "Shah Alam".into(),
            postcode: "40150".into(),
            country_iso: "my".into(),
            admin1_code: "12".into(),
            ..supplier("KEDAI-BESI", "Kedai Besi Perusahaan Sdn. Bhd.")
        },
        supplier("PACK-CO", "Packaging Co."),
    ]
}

fn tax_code(code: &str, name: &str, rate: &str) -> NewTaxCode {
    NewTaxCode {
        code: code.into(),
        name: name.into(),
        rate: rate.into(),
        ..NewTaxCode::default()
    }
}

fn tax_codes() -> [NewTaxCode; 2] {
    [
        tax_code("SST-10", "Sales tax 10%", "10"),
        tax_code("EXEMPT", "Exempt", "0"),
    ]
}

fn show<T: Debug>(step: &str, result: Result<Outcome<T>>) -> Option<T> {
    match result {
        Ok(Outcome::Saved(record)) => {
            println!("{step}: saved {record:?}");
            Some(record)
        }
        Ok(Outcome::Refused(refusal)) => {
            println!("{step}: refused ({})", refusal.text());
            None
        }
        Ok(Outcome::CompanyArchived) => {
            println!("{step}: refused (company archived)");
            None
        }
        Err(error) => {
            println!("{step}: {error}");
            None
        }
    }
}

fn line(description: &str, quantity: &str, unit: &str) -> NewRequestLine {
    NewRequestLine {
        description: description.into(),
        quantity: quantity.into(),
        unit: unit.into(),
        ..NewRequestLine::default()
    }
}

/// A record saved by a step the walkthrough cannot go on without.
fn saved<T>(record: Option<T>) -> Result<T> {
    record.ok_or(Error::Internal)
}

fn order_line(
    description: &str,
    quantity: &str,
    unit: &str,
    unit_price: &str,
    tax_code_id: Option<i64>,
) -> NewOrderLine {
    NewOrderLine {
        description: description.into(),
        quantity: quantity.into(),
        unit: unit.into(),
        unit_price: unit_price.into(),
        tax_code_id,
        ..NewOrderLine::default()
    }
}

fn decimal(value: &str) -> Decimal {
    value.parse().unwrap_or_default()
}

/// One order as a list row: number, supplier, status, lines and totals.
fn print_order(order: &Order) {
    println!(
        "  PO-{} {} [{}] {} line(s), subtotal {} tax {} total {} {}{}",
        order.order_number,
        order
            .supplier
            .as_ref()
            .map_or("?", |supplier| supplier.code.as_str()),
        order.status.as_str(),
        order.lines.len(),
        order.subtotal,
        order.tax_total,
        order.total,
        order.currency_code,
        order
            .purchase_request_number
            .map(|number| format!(", from PR-{number}"))
            .unwrap_or_default()
    );
    for line in &order.lines {
        println!(
            "    {}. {} {} {} at {} (tax {}), received {}",
            line.line_number,
            line.description,
            line.quantity,
            line.unit,
            line.unit_price,
            line.tax_rate
                .map_or_else(|| "none".to_owned(), |rate| format!("{rate}%")),
            line.received_quantity
        );
    }
}

fn print_history(order: &Order) {
    println!("PO-{} history:", order.order_number);
    for event in &order.events {
        println!(
            "  {} -> {} by user {}{}",
            event.from_status.map_or("created", |s| s.as_str()),
            event.to_status.as_str(),
            event.actor_user_id,
            event
                .reason
                .as_deref()
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default()
        );
    }
    for amendment in &order.amendments {
        println!(
            "  amendment {} by user {}: {}; added lines {:?}, superseded {:?}",
            amendment.amendment_number,
            amendment.amended_by_user_id,
            amendment.reason,
            amendment
                .lines
                .iter()
                .map(|line| line.id)
                .collect::<Vec<_>>(),
            amendment.superseded_line_ids
        );
    }
}

/// Calls the `supply_chain/purchasing.order_line` port as a consumer in
/// another container would, with its own JSON shape.
async fn port_line(
    ports: &Ports,
    client: &tokio_postgres::Client,
    scope: &Scope,
    step: &str,
    line_id: i64,
) {
    let request = json!({"company_id": COMPANY, "line_id": line_id});
    match ports
        .call::<_, Value>(
            client,
            scope,
            "supply_chain/purchasing.order_line",
            &request,
        )
        .await
    {
        Ok(line) => println!("{step}: {line}"),
        Err(PortError::Refused(error)) => println!("{step}: refused by the provider ({error})"),
        Err(error) => println!("{step}: {error}"),
    }
}

fn print_requests(requests: &[Request]) {
    for request in requests {
        println!(
            "  PR-{} {} [{}] {} line(s){}",
            request.request_number,
            request.title,
            request.status.as_str(),
            request.lines.len(),
            request
                .decision_reason
                .as_deref()
                .map(|reason| format!("; decision: {reason}"))
                .unwrap_or_default()
        );
    }
}

async fn tenant_owner_role(
    client: &impl tokio_postgres::GenericClient,
    actor: &Scope,
) -> Result<i64> {
    let page = roles::list(
        client,
        actor,
        &[],
        &Query {
            search: "tenant_owner".into(),
            sort: Sort::Code,
            descending: false,
            page: 1,
            page_size: 50,
        },
    )
    .await?;
    page.entries
        .iter()
        .find(|role| role.code.as_deref() == Some("tenant_owner"))
        .map(|role| role.id)
        .ok_or(Error::NotFound)
}

fn print_suppliers(suppliers: &[Supplier]) {
    for supplier in suppliers {
        println!(
            "  {} {} ({}; {}; {})",
            supplier.code,
            supplier.name,
            supplier.contact_name.as_deref().unwrap_or("no contact"),
            supplier
                .locality
                .as_deref()
                .map(|locality| {
                    format!(
                        "{locality} {}",
                        supplier.country_iso.as_deref().unwrap_or_default()
                    )
                })
                .unwrap_or_else(|| "no address".into()),
            if supplier.active {
                "active"
            } else {
                "inactive"
            }
        );
    }
}

fn print_tax_codes(tax_codes: &[TaxCode]) {
    for tax_code in tax_codes {
        println!(
            "  {} {} {}% ({})",
            tax_code.code,
            tax_code.name,
            tax_code.rate.normalize(),
            if tax_code.active {
                "active"
            } else {
                "inactive"
            }
        );
    }
}

/// Runs the walkthrough against `url`, a freshly created disposable database.
/// Runs a statement the facade never makes and prints PostgreSQL's answer.
async fn refused(client: &tokio_postgres::Client, step: &str, sql: &str, id: i64) {
    match client.execute(sql, &[&id]).await {
        Ok(rows) => println!("{step}: changed {rows} row(s)"),
        Err(error) => println!(
            "{step}: refused ({})",
            error
                .as_db_error()
                .map(|db| db.message().to_owned())
                .unwrap_or_else(|| error.to_string())
        ),
    }
}

pub async fn run(url: &str) -> Result<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(url)?;
    let modules = bilibi_module_registry::discover()?;
    verify_composition(
        &modules,
        bilibi_domains::MODULES,
        bilibi_extensions::MODULES,
    )?;
    let migrated = bilibi_database::migrate(&db, &modules).await?;
    println!(
        "mounted Domain modules: {:?}; {} migration(s) applied",
        bilibi_domains::MODULES,
        migrated.applied.len()
    );
    bilibi_walkthrough_fixture::install(&db).await?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.install(&modules)?;
    // Reconciliation creates each tenant's contributed system roles, among
    // them Tenant Owner with the seven Purchasing capabilities.
    let grants = services.get::<Grants>("base/authz.grants")?;
    grants.reconcile().await?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let purchasing = services.get::<Purchasing>(SERVICE)?;
    let ports = services.get::<Ports>(bilibi_ports::SERVICE)?;

    let client = db.get().await?;
    let admin = authentication
        .authenticate(&**client, TENANT, ADMIN, "walkthrough".into())
        .await?;
    let role = tenant_owner_role(&**client, &admin).await?;
    grants.assign_role(&admin, MANAGER, role, true).await?;
    println!("manager@example.test holds Tenant Owner (role {role}) in tenant {TENANT}");
    let manager = authentication
        .authenticate(&**client, TENANT, MANAGER, "walkthrough".into())
        .await?;
    let viewer = authentication
        .authenticate(&**client, TENANT, VIEWER, "walkthrough".into())
        .await?;

    for new in suppliers() {
        show(
            &format!("manager adds supplier {}", new.code),
            purchasing.create_supplier(&manager, COMPANY, &new).await,
        );
    }
    for new in tax_codes() {
        show(
            &format!("manager adds tax code {}", new.code),
            purchasing.create_tax_code(&manager, COMPANY, &new).await,
        );
    }

    // The refusals a page shows: a taken code, a broken rule and a missing
    // capability; then the manager's tenant-wide reach over a sibling company.
    show(
        "manager adds supplier PACK-CO again",
        purchasing
            .create_supplier(&manager, COMPANY, &supplier("PACK-CO", "Packaging Co."))
            .await,
    );
    show(
        "manager adds a supplier with no name and a bad email",
        purchasing
            .create_supplier(
                &manager,
                COMPANY,
                &NewSupplier {
                    email: "not an address".into(),
                    ..supplier("NONAME", "")
                },
            )
            .await,
    );
    show(
        "manager adds tax code FINE at 1.23456%",
        purchasing
            .create_tax_code(&manager, COMPANY, &tax_code("FINE", "Too fine", "1.23456"))
            .await,
    );
    show(
        "viewer adds supplier VIEWER-CO",
        purchasing
            .create_supplier(
                &viewer,
                COMPANY,
                &supplier("VIEWER-CO", "Viewer's supplier"),
            )
            .await,
    );
    show(
        &format!("manager, with tenant-wide reach, adds a supplier to company {OTHER_COMPANY}"),
        purchasing
            .create_supplier(&manager, OTHER_COMPANY, &supplier("ELSEWHERE", "Elsewhere"))
            .await,
    );
    match purchasing.list_suppliers(&viewer, COMPANY, false).await {
        Ok(found) => println!(
            "viewer lists company {COMPANY}'s suppliers: {} found",
            found.len()
        ),
        Err(error) => println!("viewer lists company {COMPANY}'s suppliers: {error}"),
    }

    // Details change and a supplier or tax code is deactivated, never deleted.
    let suppliers_now = purchasing.list_suppliers(&manager, COMPANY, false).await?;
    if let Some(pack) = suppliers_now.iter().find(|s| s.code == "PACK-CO") {
        show(
            "manager gives PACK-CO a contact and deactivates it",
            purchasing
                .update_supplier(
                    &manager,
                    COMPANY,
                    pack.id,
                    &SupplierChanges {
                        contact_name: Some("Daniel Ong".into()),
                        active: Some(false),
                        ..SupplierChanges::default()
                    },
                )
                .await,
        );
        show(
            "manager blanks PACK-CO's name",
            purchasing
                .update_supplier(
                    &manager,
                    COMPANY,
                    pack.id,
                    &SupplierChanges {
                        name: Some("  ".into()),
                        ..SupplierChanges::default()
                    },
                )
                .await,
        );
    }
    show(
        "manager changes a supplier that does not exist",
        purchasing
            .update_supplier(&manager, COMPANY, i64::MAX, &SupplierChanges::default())
            .await,
    );
    let tax_codes_now = purchasing.list_tax_codes(&manager, COMPANY, false).await?;
    if let Some(sst) = tax_codes_now.iter().find(|t| t.code == "SST-10") {
        show(
            "manager sets SST-10 to 8%",
            purchasing
                .update_tax_code(
                    &manager,
                    COMPANY,
                    sst.id,
                    &TaxCodeChanges {
                        name: Some("Sales tax 8%".into()),
                        rate: Some("8".into()),
                        ..TaxCodeChanges::default()
                    },
                )
                .await,
        );
    }
    if let Some(exempt) = tax_codes_now.iter().find(|t| t.code == "EXEMPT") {
        show(
            "manager sets EXEMPT to 101%",
            purchasing
                .update_tax_code(
                    &manager,
                    COMPANY,
                    exempt.id,
                    &TaxCodeChanges {
                        rate: Some("101".into()),
                        ..TaxCodeChanges::default()
                    },
                )
                .await,
        );
    }

    // The request lifecycle. The manager drafts, corrects and submits; the
    // admin, someone else, decides.
    show(
        "manager drafts a request with no title and a bad line",
        purchasing
            .create_request(
                &manager,
                COMPANY,
                &NewRequest {
                    needed_on: "2026-02-30".into(),
                    lines: vec![line("Steel bracket", "0", "pcs"), line("", "1.23456", "")],
                    ..NewRequest::default()
                },
            )
            .await,
    );
    let brackets = saved(show(
        "manager drafts a request for brackets",
        purchasing
            .create_request(
                &manager,
                COMPANY,
                &NewRequest {
                    title: "Brackets for line 2".into(),
                    justification: "The line 2 rebuild needs new mounts.".into(),
                    needed_on: "2026-11-30".into(),
                    lines: vec![line("Steel bracket 40 mm", "200", "pcs")],
                },
            )
            .await,
    ))?;
    show(
        "manager adds a second line to the draft",
        purchasing
            .update_request(
                &manager,
                COMPANY,
                brackets.id,
                &RequestChanges {
                    lines: Some(vec![
                        line("Steel bracket 40 mm", "200", "pcs"),
                        NewRequestLine {
                            external_item_ref: "M8-BOLT".into(),
                            ..line("M8 bolt", "400.5", "pcs")
                        },
                    ]),
                    ..RequestChanges::default()
                },
            )
            .await,
    );
    show(
        "manager submits it",
        purchasing
            .submit_request(&manager, COMPANY, brackets.id)
            .await,
    );
    show(
        "manager changes the submitted request's title",
        purchasing
            .update_request(
                &manager,
                COMPANY,
                brackets.id,
                &RequestChanges {
                    title: Some("Something else".into()),
                    ..RequestChanges::default()
                },
            )
            .await,
    );
    show(
        "manager approves the request they submitted",
        purchasing
            .approve_request(&manager, COMPANY, brackets.id, "")
            .await,
    );
    show(
        "viewer approves it",
        purchasing
            .approve_request(&viewer, COMPANY, brackets.id, "")
            .await,
    );
    show(
        "admin approves it",
        purchasing
            .approve_request(&admin, COMPANY, brackets.id, "Budgeted for Q4")
            .await,
    );

    let empty = saved(show(
        "manager drafts a request with no lines",
        purchasing
            .create_request(
                &manager,
                COMPANY,
                &NewRequest {
                    title: "Packaging".into(),
                    ..NewRequest::default()
                },
            )
            .await,
    ))?;
    show(
        "manager submits it",
        purchasing.submit_request(&manager, COMPANY, empty.id).await,
    );
    show(
        "manager adds a line",
        purchasing
            .update_request(
                &manager,
                COMPANY,
                empty.id,
                &RequestChanges {
                    lines: Some(vec![line("Carton 400x300", "50", "box")]),
                    ..RequestChanges::default()
                },
            )
            .await,
    );
    show(
        "manager submits it",
        purchasing.submit_request(&manager, COMPANY, empty.id).await,
    );
    show(
        "admin rejects it without a reason",
        purchasing
            .reject_request(&admin, COMPANY, empty.id, " ")
            .await,
    );
    show(
        "admin rejects it",
        purchasing
            .reject_request(&admin, COMPANY, empty.id, "Use the stock in bay 4")
            .await,
    );
    show(
        "manager cancels the rejected request",
        purchasing
            .cancel_request(&manager, COMPANY, empty.id, "No longer needed")
            .await,
    );

    let gloves = saved(show(
        "manager drafts a request for gloves",
        purchasing
            .create_request(
                &manager,
                COMPANY,
                &NewRequest {
                    title: "Gloves".into(),
                    lines: vec![line("Nitrile gloves, size M", "20", "box")],
                    ..NewRequest::default()
                },
            )
            .await,
    ))?;
    show(
        "viewer drafts a request",
        purchasing
            .create_request(
                &viewer,
                COMPANY,
                &NewRequest {
                    title: "Viewer's request".into(),
                    ..NewRequest::default()
                },
            )
            .await,
    );
    show(
        "manager cancels the gloves draft",
        purchasing
            .cancel_request(&manager, COMPANY, gloves.id, "Ordered with the brackets")
            .await,
    );

    // The order lifecycle. The approved bracket request is ordered from an
    // active supplier, priced on the draft, submitted by the manager and
    // approved by the admin, sent, received and closed.
    let suppliers_now = purchasing.list_suppliers(&manager, COMPANY, false).await?;
    let code = |code: &str| suppliers_now.iter().find(|s| s.code == code).map(|s| s.id);
    let tax_codes_now = purchasing.list_tax_codes(&manager, COMPANY, false).await?;
    let sst = tax_codes_now
        .iter()
        .find(|t| t.code == "SST-10")
        .map(|t| t.id);
    let from_request = |supplier_id, currency: &str| OrderFromRequest {
        supplier_id,
        currency_code: currency.into(),
        reference: "Q-2026-118".into(),
        expected_on: "2026-12-01".into(),
        ..OrderFromRequest::default()
    };
    show(
        "manager orders the bracket request from the inactive PACK-CO",
        purchasing
            .create_order_from_request(
                &manager,
                COMPANY,
                brackets.id,
                &from_request(code("PACK-CO"), "MYR"),
            )
            .await,
    );
    show(
        "manager orders it in currency 'ringgit'",
        purchasing
            .create_order_from_request(
                &manager,
                COMPANY,
                brackets.id,
                &from_request(code("KEDAI-BESI"), "ringgit"),
            )
            .await,
    );
    show(
        "manager orders the cancelled packaging request",
        purchasing
            .create_order_from_request(
                &manager,
                COMPANY,
                empty.id,
                &from_request(code("KEDAI-BESI"), "MYR"),
            )
            .await,
    );
    let bracket_order = saved(show(
        "manager orders the bracket request from KEDAI-BESI",
        purchasing
            .create_order_from_request(
                &manager,
                COMPANY,
                brackets.id,
                &from_request(code("KEDAI-BESI"), "myr"),
            )
            .await,
    ))?;
    print_order(&bracket_order);
    show(
        "manager prices the draft with a tax code that does not exist",
        purchasing
            .update_order(
                &manager,
                COMPANY,
                bracket_order.id,
                &OrderChanges {
                    lines: Some(vec![order_line(
                        "Steel bracket 40 mm",
                        "200",
                        "pcs",
                        "-1",
                        Some(i64::MAX),
                    )]),
                    ..OrderChanges::default()
                },
            )
            .await,
    );
    let priced = vec![
        order_line("Steel bracket 40 mm", "200", "pcs", "4.25", sst),
        NewOrderLine {
            external_item_ref: "M8-BOLT".into(),
            ..order_line("M8 bolt", "400.5", "pcs", "0.18", sst)
        },
    ];
    show(
        "manager prices the draft",
        purchasing
            .update_order(
                &manager,
                COMPANY,
                bracket_order.id,
                &OrderChanges {
                    lines: Some(priced),
                    notes: Some("Deliver to bay 2".into()),
                    ..OrderChanges::default()
                },
            )
            .await,
    )
    .iter()
    .for_each(print_order);
    show(
        "manager submits it",
        purchasing
            .submit_order(&manager, COMPANY, bracket_order.id)
            .await,
    );
    show(
        "manager approves the order they submitted",
        purchasing
            .approve_order(&manager, COMPANY, bracket_order.id, "")
            .await,
    );
    show(
        "admin returns it without a reason",
        purchasing
            .return_order(&admin, COMPANY, bracket_order.id, "")
            .await,
    );
    show(
        "admin returns it",
        purchasing
            .return_order(
                &admin,
                COMPANY,
                bracket_order.id,
                "Add the delivery bay to the reference",
            )
            .await,
    );
    show(
        "manager corrects the reference",
        purchasing
            .update_order(
                &manager,
                COMPANY,
                bracket_order.id,
                &OrderChanges {
                    reference: Some("Q-2026-118 / bay 2".into()),
                    ..OrderChanges::default()
                },
            )
            .await,
    );
    show(
        "manager resubmits it",
        purchasing
            .submit_order(&manager, COMPANY, bracket_order.id)
            .await,
    );
    show(
        "viewer approves it",
        purchasing
            .approve_order(&viewer, COMPANY, bracket_order.id, "")
            .await,
    );
    show(
        "admin approves it",
        purchasing
            .approve_order(&admin, COMPANY, bracket_order.id, "Within budget")
            .await,
    );
    show(
        "manager records a receipt against the approved, unsent order",
        purchasing
            .record_receipt(&manager, COMPANY, bracket_order.id, &[], Decimal::ZERO)
            .await,
    );
    let sent = saved(show(
        "manager sends it",
        purchasing
            .send_order(&manager, COMPANY, bracket_order.id)
            .await,
    ))?;
    let (bracket_line, bolt_line) = (sent.lines[0].id, sent.lines[1].id);
    match purchasing
        .find_order_by_number(&manager, COMPANY, sent.order_number)
        .await?
    {
        Some(found) => println!(
            "manager finds PO-{} by number: id {}, {}",
            found.order_number,
            found.id,
            found.status.as_str()
        ),
        None => println!("manager finds PO-{}: not found", sent.order_number),
    }
    match purchasing
        .get_order_line(&manager, COMPANY, bolt_line)
        .await?
    {
        Some(facts) => println!(
            "manager reads order line {}: PO-{} [{}] supplier {} {}, {} {} at {}",
            facts.line.id,
            facts.order_number,
            facts.order_status.as_str(),
            facts.supplier_id,
            facts.currency_code,
            facts.line.quantity,
            facts.line.unit,
            facts.line.unit_price
        ),
        None => println!("manager reads order line {bolt_line}: not found"),
    }
    let port_client = db.get().await?;
    port_line(
        &ports,
        &port_client,
        &manager,
        "a consumer reads the bolt line through the port",
        bolt_line,
    )
    .await;
    port_line(
        &ports,
        &port_client,
        &viewer,
        "viewer reads it through the port",
        bolt_line,
    )
    .await;
    port_line(
        &ports,
        &port_client,
        &manager,
        "a consumer reads a line that does not exist",
        i64::MAX,
    )
    .await;
    match ports
        .call_json(
            &**port_client,
            &manager,
            "supply_chain/purchasing.order_line",
            json!({"line": bolt_line}),
        )
        .await
    {
        Ok(answer) => println!("a consumer sends another request shape: {answer}"),
        Err(error) => println!("a consumer sends another request shape: {error}"),
    }

    // Receiving's seam: goods recorded against the sent order, within the
    // tolerance the caller states, and taken back by a reversal.
    let receipt = |line_id, quantity: &str| ReceiptLine {
        line_id,
        quantity: decimal(quantity),
    };
    show(
        "viewer records a receipt",
        purchasing
            .record_receipt(
                &viewer,
                COMPANY,
                bracket_order.id,
                &[receipt(bracket_line, "1")],
                Decimal::ZERO,
            )
            .await,
    );
    show(
        "manager receives 210 brackets with no tolerance",
        purchasing
            .record_receipt(
                &manager,
                COMPANY,
                bracket_order.id,
                &[receipt(bracket_line, "210")],
                Decimal::ZERO,
            )
            .await,
    );
    show(
        "manager receives 210 brackets within a 2% tolerance",
        purchasing
            .record_receipt(
                &manager,
                COMPANY,
                bracket_order.id,
                &[receipt(bracket_line, "210")],
                decimal("2"),
            )
            .await,
    );
    show(
        "manager names the bracket line twice",
        purchasing
            .record_receipt(
                &manager,
                COMPANY,
                bracket_order.id,
                &[receipt(bracket_line, "1"), receipt(bracket_line, "1")],
                Decimal::ZERO,
            )
            .await,
    );
    show(
        "manager receives 120 brackets",
        purchasing
            .record_receipt(
                &manager,
                COMPANY,
                bracket_order.id,
                &[receipt(bracket_line, "120")],
                Decimal::ZERO,
            )
            .await,
    );
    show(
        "manager cancels the partially received order",
        purchasing
            .cancel_order(&manager, COMPANY, bracket_order.id, "Supplier late")
            .await,
    );
    show(
        "manager reverses 150 brackets",
        purchasing
            .reverse_receipt(
                &manager,
                COMPANY,
                bracket_order.id,
                &[receipt(bracket_line, "150")],
                "Damaged in transit",
            )
            .await,
    );
    show(
        "manager reverses 20 damaged brackets",
        purchasing
            .reverse_receipt(
                &manager,
                COMPANY,
                bracket_order.id,
                &[receipt(bracket_line, "20")],
                "Damaged in transit",
            )
            .await,
    );
    show(
        "manager receives the remaining brackets and every bolt",
        purchasing
            .record_receipt(
                &manager,
                COMPANY,
                bracket_order.id,
                &[receipt(bracket_line, "100"), receipt(bolt_line, "400.5")],
                Decimal::ZERO,
            )
            .await,
    )
    .iter()
    .for_each(print_order);
    show(
        "manager reverses 10 bolts against the closed order",
        purchasing
            .reverse_receipt(
                &manager,
                COMPANY,
                bracket_order.id,
                &[receipt(bolt_line, "10")],
                "Returned short",
            )
            .await,
    );
    show(
        "manager closes it short",
        purchasing
            .close_order(&manager, COMPANY, bracket_order.id, "Ten bolts written off")
            .await,
    );

    // An amendment: one that changes the header alone keeps the order sent;
    // one that changes a line sends it back for a fresh approval by someone
    // other than the amender.
    let gloves_order = saved(show(
        "manager drafts an order for gloves",
        purchasing
            .create_order(
                &manager,
                COMPANY,
                &NewOrder {
                    supplier_id: code("KEDAI-BESI"),
                    currency_code: "MYR".into(),
                    lines: vec![order_line(
                        "Nitrile gloves, size M",
                        "20",
                        "box",
                        "32.50",
                        None,
                    )],
                    ..NewOrder::default()
                },
            )
            .await,
    ))?;
    show(
        "manager submits it",
        purchasing
            .submit_order(&manager, COMPANY, gloves_order.id)
            .await,
    );
    show(
        "manager amends the submitted order",
        purchasing
            .amend_order(
                &manager,
                COMPANY,
                gloves_order.id,
                &OrderAmendment::default(),
            )
            .await,
    );
    show(
        "admin approves it",
        purchasing
            .approve_order(&admin, COMPANY, gloves_order.id, "")
            .await,
    );
    show(
        "manager sends it",
        purchasing
            .send_order(&manager, COMPANY, gloves_order.id)
            .await,
    );
    let gloves_lines = vec![order_line(
        "Nitrile gloves, size M",
        "20",
        "box",
        "32.50",
        None,
    )];
    show(
        "manager amends it without a reason",
        purchasing
            .amend_order(
                &manager,
                COMPANY,
                gloves_order.id,
                &OrderAmendment {
                    lines: gloves_lines.clone(),
                    ..OrderAmendment::default()
                },
            )
            .await,
    );
    show(
        "manager amends the expected date only",
        purchasing
            .amend_order(
                &manager,
                COMPANY,
                gloves_order.id,
                &OrderAmendment {
                    reason: "Supplier confirmed delivery".into(),
                    expected_on: Some("2026-11-20".into()),
                    lines: gloves_lines,
                    ..OrderAmendment::default()
                },
            )
            .await,
    );
    show(
        "manager amends the quantity to 30 boxes",
        purchasing
            .amend_order(
                &manager,
                COMPANY,
                gloves_order.id,
                &OrderAmendment {
                    reason: "Second shift starts in November".into(),
                    lines: vec![order_line(
                        "Nitrile gloves, size M",
                        "30",
                        "box",
                        "32.50",
                        sst,
                    )],
                    ..OrderAmendment::default()
                },
            )
            .await,
    );
    show(
        "manager approves the amended order",
        purchasing
            .approve_order(&manager, COMPANY, gloves_order.id, "")
            .await,
    );
    show(
        "admin approves it",
        purchasing
            .approve_order(&admin, COMPANY, gloves_order.id, "Second shift budgeted")
            .await,
    );
    show(
        "manager cancels it without a reason",
        purchasing
            .cancel_order(&manager, COMPANY, gloves_order.id, "")
            .await,
    );
    show(
        "manager cancels it",
        purchasing
            .cancel_order(
                &manager,
                COMPANY,
                gloves_order.id,
                "Gloves come from the group contract",
            )
            .await,
    );
    show(
        "manager drafts an order with no supplier and a bad line",
        purchasing
            .create_order(
                &manager,
                COMPANY,
                &NewOrder {
                    currency_code: "MYR".into(),
                    expected_on: "tomorrow".into(),
                    lines: vec![order_line("", "1", "pcs", "9999999999999999", None)],
                    ..NewOrder::default()
                },
            )
            .await,
    );
    show(
        "viewer drafts an order",
        purchasing
            .create_order(&viewer, COMPANY, &NewOrder::default())
            .await,
    );

    // PostgreSQL refuses what the facade never attempts on an order.
    for (step, sql) in [
        (
            "SQL reprices the closed order's lines",
            "UPDATE supply_chain_purchasing_order_lines SET unit_price=0 WHERE order_id=$1",
        ),
        (
            "SQL deletes the closed order's lines",
            "DELETE FROM supply_chain_purchasing_order_lines WHERE order_id=$1",
        ),
        (
            "SQL reopens the closed order as sent",
            "UPDATE supply_chain_purchasing_orders SET status='sent' WHERE id=$1",
        ),
        (
            "SQL rewrites the closed order's history",
            "UPDATE supply_chain_purchasing_order_events SET reason='Rewritten' WHERE order_id=$1",
        ),
    ] {
        refused(&client, step, sql, bracket_order.id).await;
    }
    refused(
        &client,
        "SQL rewrites the gloves order's amendment",
        "UPDATE supply_chain_purchasing_order_amendments SET reason='Rewritten' WHERE order_id=$1",
        gloves_order.id,
    )
    .await;
    for order in [bracket_order.id, gloves_order.id] {
        if let Some(order) = purchasing.get_order(&manager, COMPANY, order).await? {
            print_history(&order);
        }
    }
    println!("company {COMPANY}'s purchase orders:");
    for order in &purchasing
        .list_orders(&manager, COMPANY, OrderQuery::default())
        .await?
    {
        print_order(order);
    }

    // Requests someone other than the manager submitted, left for the
    // manager to decide on the Purchase requests page.
    for (title, lines) in [
        (
            "Forklift service",
            vec![line("Forklift annual service", "1", "visit")],
        ),
        (
            "Pallet wrap",
            vec![
                line("Stretch film 500 mm", "24", "roll"),
                line("Corner boards", "200", "pcs"),
            ],
        ),
    ] {
        let request = saved(show(
            &format!("admin drafts {title}"),
            purchasing
                .create_request(
                    &admin,
                    COMPANY,
                    &NewRequest {
                        title: title.into(),
                        lines,
                        ..NewRequest::default()
                    },
                )
                .await,
        ))?;
        show(
            &format!("admin submits {title}"),
            purchasing.submit_request(&admin, COMPANY, request.id).await,
        );
    }

    // PostgreSQL itself refuses what the facade never attempts: a change to
    // a submitted request's header, and any change to its history.
    for (step, sql) in [
        (
            "SQL renames the approved request",
            "UPDATE supply_chain_purchasing_requests SET title='Renamed' WHERE id=$1",
        ),
        (
            "SQL rewrites the approved request's history",
            "UPDATE supply_chain_purchasing_request_events SET reason='Rewritten' WHERE request_id=$1",
        ),
        (
            "SQL deletes the approved request's lines",
            "DELETE FROM supply_chain_purchasing_request_lines WHERE request_id=$1",
        ),
        (
            "SQL cancels the approved request without a history row",
            "UPDATE supply_chain_purchasing_requests SET status='cancelled' WHERE id=$1",
        ),
    ] {
        refused(&client, step, sql, brackets.id).await;
    }

    if let Some(request) = purchasing
        .get_request(&manager, COMPANY, brackets.id)
        .await?
    {
        println!("PR-{} history:", request.request_number);
        for event in &request.events {
            println!(
                "  {} -> {} by user {}{}",
                event.from_status.map(|s| s.as_str()).unwrap_or("created"),
                event.to_status.as_str(),
                event.actor_user_id,
                event
                    .reason
                    .as_deref()
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default()
            );
        }
    }
    println!("company {COMPANY}'s purchase requests:");
    print_requests(
        &purchasing
            .list_requests(&manager, COMPANY, RequestQuery::default())
            .await?,
    );
    println!("company {COMPANY}'s suppliers:");
    print_suppliers(&purchasing.list_suppliers(&manager, COMPANY, false).await?);
    println!("company {COMPANY}'s tax codes:");
    print_tax_codes(&purchasing.list_tax_codes(&manager, COMPANY, false).await?);
    agent::run(&db, &services, &purchasing, &admin)
        .await
        .map_err(|error| {
            eprintln!("H7 walkthrough failed: {error}");
            Error::Internal
        })?;
    receiving::run(&services, &db, &manager, &admin, &viewer).await?;
    order_events::run(&services, &db, &manager, &admin, &viewer).await?;
    println!(
        "Run bilibi-web against this database and sign in as manager@example.test (password {}): the Supply chain menu offers Purchase requests, Purchase orders and Suppliers, and the Purchase requests page leaves Forklift service and Pallet wrap for the manager to decide. viewer@example.test sees no Supply chain menu.",
        bilibi_walkthrough_fixture::PASSWORD
    );
    Ok(())
}
