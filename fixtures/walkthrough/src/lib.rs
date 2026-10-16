//! `DATABASE_URL=… cargo run -p bilibi-supply-chain-walkthrough`: the Supply
//! Chain walkthrough fixture for slice proofs. Against a freshly created
//! disposable database it migrates the composed app, installs the Platform
//! walkthrough fixture (its tenants, accounts and companies), gives the
//! manager the Tenant Owner role that carries every Purchasing capability,
//! and creates Company 001's first suppliers and tax codes through the
//! Purchasing facade, showing the refusals beside the writes. A
//! `bilibi-web run` against the same database then shows the Supply chain
//! menu to `manager@example.test` and withholds it from `viewer@example.test`.
//!
//! Never a product dependency, and never run against a real install: the
//! Platform fixture refuses a database that already holds other accounts.
use bilibi_authz::{
    grants::Grants,
    roles::{self, Query, Sort},
};
use bilibi_database::{pool, Error, Result};
use bilibi_module_registry::{verify_composition, Services};
use bilibi_supply_chain_purchasing::{
    NewSupplier, NewTaxCode, Outcome, Purchasing, Supplier, TaxCode, SERVICE,
};
use bilibi_tenancy::{Authentication, Scope};
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

fn show<T: Debug>(step: &str, result: Result<Outcome<T>>) {
    match result {
        Ok(Outcome::Saved(record)) => println!("{step}: saved {record:?}"),
        Ok(Outcome::Refused(errors)) => {
            let errors: Vec<_> = errors
                .iter()
                .map(|error| format!("{} {}", error.field, error.message))
                .collect();
            println!("{step}: refused ({})", errors.join("; "))
        }
        Ok(Outcome::CompanyArchived) => println!("{step}: refused (company archived)"),
        Err(error) => println!("{step}: {error}"),
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

    println!("company {COMPANY}'s suppliers:");
    print_suppliers(&purchasing.list_suppliers(&manager, COMPANY, false).await?);
    println!("company {COMPANY}'s tax codes:");
    print_tax_codes(&purchasing.list_tax_codes(&manager, COMPANY, false).await?);
    println!(
        "Run bilibi-web against this database and sign in as manager@example.test (password {}): the Supply chain menu offers Purchase requests, Purchase orders and Suppliers. viewer@example.test sees no Supply chain menu.",
        bilibi_walkthrough_fixture::PASSWORD
    );
    Ok(())
}
