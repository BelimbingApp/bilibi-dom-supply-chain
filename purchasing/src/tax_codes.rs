//! Tax codes: Bilimbi's `TaxCode` changeset rules and the table's SQL. A
//! company's tax codes are its configuration; an order line snapshots the
//! rate it was priced with, so a later rate change leaves posted orders as
//! they were. Private to the facade.
use crate::validate::{required_text, Checks, BLANK, INVALID, TAKEN};
use crate::{FieldError, NewTaxCode, Outcome, Refusal, TaxCode, TaxCodeChanges};
use bilibi_database::{Change, MutationTransaction, Result};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use tokio_postgres::{GenericClient, Row};

const COLUMNS: &str = "id,code,name,rate,active";

fn from_row(row: &Row) -> Result<TaxCode> {
    Ok(TaxCode {
        id: row.try_get(0)?,
        code: row.try_get(1)?,
        name: row.try_get(2)?,
        rate: row.try_get(3)?,
        active: row.try_get(4)?,
    })
}

fn to_json(tax_code: &TaxCode) -> Value {
    json!({
        "id": tax_code.id, "code": tax_code.code, "name": tax_code.name,
        "rate": tax_code.rate.to_string(), "active": tax_code.active,
    })
}

pub(crate) async fn list(
    client: &impl GenericClient,
    tenant: i64,
    company_id: i64,
    active_only: bool,
) -> Result<Vec<TaxCode>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM supply_chain_purchasing_tax_codes WHERE tenant_id=$1 AND company_id=$2{} ORDER BY code",
        if active_only { " AND active" } else { "" }
    );
    client
        .query(&sql, &[&tenant, &company_id])
        .await?
        .iter()
        .map(from_row)
        .collect()
}

/// A percentage from 0 to 100 with up to four decimal places, refused rather
/// than rounded when finer (`validate_number` and `validate_scale` in the source).
fn rate(value: &str) -> std::result::Result<Decimal, FieldError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(FieldError::new("rate", BLANK));
    }
    let rate = Decimal::from_str_exact(value).map_err(|_| FieldError::new("rate", INVALID))?;
    if rate < Decimal::ZERO {
        return Err(FieldError::new(
            "rate",
            "must be greater than or equal to 0",
        ));
    }
    if rate > Decimal::ONE_HUNDRED {
        return Err(FieldError::new("rate", "must be less than or equal to 100"));
    }
    if rate.round_dp(4) != rate {
        return Err(FieldError::new("rate", "has more than 4 decimal places"));
    }
    Ok(rate)
}

#[derive(Default)]
struct Valid {
    code: String,
    name: String,
    rate: Decimal,
}

fn validate(input: &NewTaxCode) -> std::result::Result<Valid, Vec<FieldError>> {
    let mut checks = Checks::default();
    let valid = Valid {
        code: checks.take(required_text("code", &input.code, 20)),
        name: checks.take(required_text("name", &input.name, 120)),
        rate: checks.take(rate(&input.rate)),
    };
    checks.finish().map(|()| valid)
}

async fn code_taken(client: &impl GenericClient, company_id: i64, code: &str) -> Result<bool> {
    Ok(client
        .query_opt(
            "SELECT 1 FROM supply_chain_purchasing_tax_codes WHERE company_id=$1 AND code=$2",
            &[&company_id, &code],
        )
        .await?
        .is_some())
}

pub(crate) async fn create(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    input: &NewTaxCode,
) -> Result<Outcome<TaxCode>> {
    let valid = match validate(input) {
        Ok(valid) => valid,
        Err(errors) => return Ok(Outcome::Refused(Refusal::Invalid(errors))),
    };
    if code_taken(tx.sql(), company_id, &valid.code).await? {
        return Ok(Outcome::Refused(Refusal::Invalid(vec![FieldError::new(
            "code", TAKEN,
        )])));
    }
    let active = input.active;
    let created = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql
                    .query_one(
                        &format!(
                            "INSERT INTO supply_chain_purchasing_tax_codes(tenant_id,company_id,code,name,rate,active,inserted_at,updated_at) \
                             VALUES($1,$2,$3,$4,$5,$6,now() AT TIME ZONE 'UTC',now() AT TIME ZONE 'UTC') RETURNING {COLUMNS}"
                        ),
                        &[&tenant, &company_id, &valid.code, &valid.name, &valid.rate, &active],
                    )
                    .await?;
                let created = from_row(&row)?;
                let change = Change {
                    kind: "supply_chain.purchasing.tax_code".into(),
                    id: created.id,
                    old: json!({}),
                    new: to_json(&created),
                };
                Ok((created, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(created))
}

async fn get(
    client: &impl GenericClient,
    tenant: i64,
    company_id: i64,
    tax_code_id: i64,
) -> Result<Option<TaxCode>> {
    client
        .query_opt(
            &format!("SELECT {COLUMNS} FROM supply_chain_purchasing_tax_codes WHERE tenant_id=$1 AND company_id=$2 AND id=$3"),
            &[&tenant, &company_id, &tax_code_id],
        )
        .await?
        .as_ref()
        .map(from_row)
        .transpose()
}

pub(crate) async fn update(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    company_id: i64,
    tax_code_id: i64,
    changes: &TaxCodeChanges,
) -> Result<Outcome<TaxCode>> {
    let Some(current) = get(tx.sql(), tenant, company_id, tax_code_id).await? else {
        return Ok(Outcome::Refused(Refusal::TaxCodeNotFound));
    };
    let mut checks = Checks::default();
    let mut next = current.clone();
    if let Some(name) = &changes.name {
        next.name = checks.take(required_text("name", name, 120));
    }
    if let Some(value) = &changes.rate {
        next.rate = checks.take(rate(value));
    }
    if let Some(active) = changes.active {
        next.active = active;
    }
    if let Err(errors) = checks.finish() {
        return Ok(Outcome::Refused(Refusal::Invalid(errors)));
    }
    let updated = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql
                    .query_one(
                        &format!(
                            "UPDATE supply_chain_purchasing_tax_codes SET name=$3,rate=$4,active=$5,updated_at=now() AT TIME ZONE 'UTC' \
                             WHERE company_id=$1 AND id=$2 RETURNING {COLUMNS}"
                        ),
                        &[&company_id, &current.id, &next.name, &next.rate, &next.active],
                    )
                    .await?;
                let updated = from_row(&row)?;
                let change = Change {
                    kind: "supply_chain.purchasing.tax_code".into(),
                    id: updated.id,
                    old: to_json(&current),
                    new: to_json(&updated),
                };
                Ok((updated, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(updated))
}
