//! Suppliers: Bilimbi's `Supplier` changeset rules and the table's SQL. The
//! address fields follow Core Address's shape (lines, locality, postcode,
//! country ISO code, first-level division code) so a later move to a shared
//! party record is a copy, not a translation. Private to the facade.
use crate::validate::{country_iso, email, optional_text, required_text, Checks, TAKEN};
use crate::{FieldError, NewSupplier, Outcome, Supplier};
use bilibi_database::{Change, MutationTransaction, Result};
use serde_json::{json, Value};
use tokio_postgres::{GenericClient, Row};

const COLUMNS: &str = "id,code,name,contact_name,email,phone,address_line1,address_line2,address_line3,locality,postcode,country_iso,admin1_code,active,inserted_at,updated_at";

fn from_row(row: &Row) -> Result<Supplier> {
    Ok(Supplier {
        id: row.try_get(0)?,
        code: row.try_get(1)?,
        name: row.try_get(2)?,
        contact_name: row.try_get(3)?,
        email: row.try_get(4)?,
        phone: row.try_get(5)?,
        address_line1: row.try_get(6)?,
        address_line2: row.try_get(7)?,
        address_line3: row.try_get(8)?,
        locality: row.try_get(9)?,
        postcode: row.try_get(10)?,
        country_iso: row.try_get(11)?,
        admin1_code: row.try_get(12)?,
        active: row.try_get(13)?,
        inserted_at: row.try_get(14)?,
        updated_at: row.try_get(15)?,
    })
}

/// The audit view of a supplier: its facts, not its bookkeeping stamps.
fn to_json(supplier: &Supplier) -> Value {
    json!({
        "id": supplier.id, "code": supplier.code, "name": supplier.name,
        "contact_name": supplier.contact_name, "email": supplier.email, "phone": supplier.phone,
        "address_line1": supplier.address_line1, "address_line2": supplier.address_line2,
        "address_line3": supplier.address_line3, "locality": supplier.locality,
        "postcode": supplier.postcode, "country_iso": supplier.country_iso,
        "admin1_code": supplier.admin1_code, "active": supplier.active,
    })
}

pub(crate) async fn list(
    client: &impl GenericClient,
    tenant: i64,
    company_id: i64,
    active_only: bool,
) -> Result<Vec<Supplier>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM supply_chain_purchasing_suppliers WHERE tenant_id=$1 AND company_id=$2{} ORDER BY code",
        if active_only { " AND active" } else { "" }
    );
    client
        .query(&sql, &[&tenant, &company_id])
        .await?
        .iter()
        .map(from_row)
        .collect()
}

/// A supplier as `create_changeset/2` accepts it.
#[derive(Default)]
struct Valid {
    code: String,
    name: String,
    contact_name: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    address_line1: Option<String>,
    address_line2: Option<String>,
    address_line3: Option<String>,
    locality: Option<String>,
    postcode: Option<String>,
    country_iso: Option<String>,
    admin1_code: Option<String>,
}

fn validate(input: &NewSupplier) -> std::result::Result<Valid, Vec<FieldError>> {
    let mut checks = Checks::default();
    let valid = Valid {
        code: checks.take(required_text("code", &input.code, 50)),
        name: checks.take(required_text("name", &input.name, 200)),
        contact_name: checks.take(optional_text("contact_name", &input.contact_name, 200)),
        email: checks.take(email("email", &input.email, 255)),
        phone: checks.take(optional_text("phone", &input.phone, 50)),
        address_line1: checks.take(optional_text("address_line1", &input.address_line1, 255)),
        address_line2: checks.take(optional_text("address_line2", &input.address_line2, 255)),
        address_line3: checks.take(optional_text("address_line3", &input.address_line3, 255)),
        locality: checks.take(optional_text("locality", &input.locality, 120)),
        postcode: checks.take(optional_text("postcode", &input.postcode, 20)),
        country_iso: checks.take(country_iso("country_iso", &input.country_iso)),
        admin1_code: checks.take(optional_text("admin1_code", &input.admin1_code, 20)),
    };
    checks.finish().map(|()| valid)
}

/// The caller holds the company's writable lock, so this read decides the
/// unique `(company_id, code)` constraint before the insert can hit it.
async fn code_taken(client: &impl GenericClient, company_id: i64, code: &str) -> Result<bool> {
    Ok(client
        .query_opt(
            "SELECT 1 FROM supply_chain_purchasing_suppliers WHERE company_id=$1 AND code=$2",
            &[&company_id, &code],
        )
        .await?
        .is_some())
}

pub(crate) async fn create(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    input: &NewSupplier,
) -> Result<Outcome<Supplier>> {
    let valid = match validate(input) {
        Ok(valid) => valid,
        Err(errors) => return Ok(Outcome::Refused(errors)),
    };
    if code_taken(tx.sql(), company_id, &valid.code).await? {
        return Ok(Outcome::Refused(vec![FieldError::new("code", TAKEN)]));
    }
    let active = input.active;
    let created = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql
                    .query_one(
                        &format!(
                            "INSERT INTO supply_chain_purchasing_suppliers(tenant_id,company_id,code,name,contact_name,email,phone,address_line1,address_line2,address_line3,locality,postcode,country_iso,admin1_code,active,inserted_at,updated_at) \
                             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,now() AT TIME ZONE 'UTC',now() AT TIME ZONE 'UTC') RETURNING {COLUMNS}"
                        ),
                        &[
                            &tenant, &company_id, &valid.code, &valid.name, &valid.contact_name,
                            &valid.email, &valid.phone, &valid.address_line1, &valid.address_line2,
                            &valid.address_line3, &valid.locality, &valid.postcode, &valid.country_iso,
                            &valid.admin1_code, &active,
                        ],
                    )
                    .await?;
                let created = from_row(&row)?;
                let change = Change {
                    kind: "supply_chain.purchasing.supplier".into(),
                    id: created.id,
                    old: Value::Null,
                    new: to_json(&created),
                };
                Ok((created, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(created))
}
