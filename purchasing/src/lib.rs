//! Purchasing (`supply_chain/purchasing`): a company's suppliers and tax
//! codes, and from later versions its purchase requests and purchase orders.
//!
//! This is the module's one facade. Other modules read and write through
//! [`Purchasing`], resolved from the registry as [`SERVICE`]; the schema, SQL
//! and validation stay private. Every call takes the sealed
//! [`Scope`] it was handed and an explicit company ID. Reads authorize
//! [`VIEW`] and writes their own capability for the scope's actor and the
//! target company at the moment they run (Core Company's
//! `authorize_target`: the actor's own company, or a sibling company with
//! tenant-wide reach), so a missing grant is `Error::Forbidden` and a
//! company the scope cannot see is `Error::NotFound`. A write then holds the
//! company's writable lock inside its audited mutation transaction, so an
//! archived company is [`Outcome::CompanyArchived`] and one company's
//! purchasing writes run one at a time. See `README.md`.
mod contributions;
mod suppliers;
mod tax_codes;
mod validate;

pub use contributions::{
    APPROVE_ORDERS, APPROVE_REQUESTS, CAPABILITIES, CONFIGURE, MANAGE_ORDERS, MANAGE_REQUESTS,
    RECEIVE_ORDERS, VIEW,
};

use bilibi_company::{organisation, WriteError};
use bilibi_database::{Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use rust_decimal::Decimal;
use std::{sync::Arc, time::SystemTime};
use tokio_postgres::{Client, GenericClient};

/// The module ID, Bilimbi's, kept because IDs are persisted data.
pub const MODULE: &str = "supply_chain/purchasing";
/// The registry service ID of the [`Purchasing`] facade.
pub const SERVICE: &str = "supply_chain/purchasing.purchasing";

/// One refusal of a submitted value, in the words a form shows beside the field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}
impl FieldError {
    pub fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

/// How a write ended. A refusal is an outcome a page names, never an `Error`.
#[derive(Debug)]
pub enum Outcome<T> {
    Saved(T),
    /// The input broke a rule; one message per field.
    Refused(Vec<FieldError>),
    /// The company is archived and read-only (Bilimbi `{:error, :company_archived}`).
    CompanyArchived,
}

/// A party the company buys from: a code unique in the company and fixed for
/// good, a name, and optional contact and address facts. Deactivated, never
/// deleted.
#[derive(Clone, Debug)]
pub struct Supplier {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub contact_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub address_line3: Option<String>,
    pub locality: Option<String>,
    pub postcode: Option<String>,
    pub country_iso: Option<String>,
    pub admin1_code: Option<String>,
    pub active: bool,
    pub inserted_at: SystemTime,
    pub updated_at: SystemTime,
}

/// A supplier as a form submits it: texts are trimmed, and a blank optional
/// text is stored as absent. `active` defaults to true, as the schema does.
#[derive(Clone, Debug)]
pub struct NewSupplier {
    pub code: String,
    pub name: String,
    pub contact_name: String,
    pub email: String,
    pub phone: String,
    pub address_line1: String,
    pub address_line2: String,
    pub address_line3: String,
    pub locality: String,
    pub postcode: String,
    pub country_iso: String,
    pub admin1_code: String,
    pub active: bool,
}
impl Default for NewSupplier {
    fn default() -> Self {
        Self {
            code: String::new(),
            name: String::new(),
            contact_name: String::new(),
            email: String::new(),
            phone: String::new(),
            address_line1: String::new(),
            address_line2: String::new(),
            address_line3: String::new(),
            locality: String::new(),
            postcode: String::new(),
            country_iso: String::new(),
            admin1_code: String::new(),
            active: true,
        }
    }
}

/// A company's tax code: a code fixed for good, a name, and a rate in
/// percent with up to four decimal places.
#[derive(Clone, Debug)]
pub struct TaxCode {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub rate: Decimal,
    pub active: bool,
}

/// A tax code as a form submits it; `rate` is the text typed, validated here.
#[derive(Clone, Debug)]
pub struct NewTaxCode {
    pub code: String,
    pub name: String,
    pub rate: String,
    pub active: bool,
}
impl Default for NewTaxCode {
    fn default() -> Self {
        Self {
            code: String::new(),
            name: String::new(),
            rate: String::new(),
            active: true,
        }
    }
}

/// The facade (Bilimbi `Bilimbi.SupplyChain.Purchasing`).
pub struct Purchasing {
    db: Db,
    mutations: Arc<MutationBoundary>,
}
impl Purchasing {
    pub(crate) fn new(db: Db, mutations: Arc<MutationBoundary>) -> Self {
        Self { db, mutations }
    }

    /// The company's suppliers ordered by code; `active_only` keeps the active
    /// ones. Capability [`VIEW`].
    pub async fn list_suppliers(
        &self,
        scope: &Scope,
        company_id: i64,
        active_only: bool,
    ) -> Result<Vec<Supplier>> {
        let client = self.db.get().await?;
        authorize(&**client, scope, company_id, VIEW).await?;
        suppliers::list(&**client, scope.tenant(), company_id, active_only).await
    }

    /// Adds a supplier: `code` (unique in the company, fixed for good), `name`,
    /// and optional contact and address facts. Capability [`CONFIGURE`].
    pub async fn create_supplier(
        &self,
        scope: &Scope,
        company_id: i64,
        supplier: &NewSupplier,
    ) -> Result<Outcome<Supplier>> {
        let mut client = self.db.get().await?;
        let Some(mut tx) = self
            .begin(
                &mut client,
                scope,
                company_id,
                CONFIGURE,
                "supply_chain.purchasing.supplier.create",
            )
            .await?
        else {
            return Ok(Outcome::CompanyArchived);
        };
        let outcome = suppliers::create(&mut tx, scope.tenant(), company_id, supplier).await?;
        finish(tx, &outcome).await?;
        Ok(outcome)
    }

    /// The company's tax codes ordered by code; `active_only` keeps the active
    /// ones. Capability [`VIEW`].
    pub async fn list_tax_codes(
        &self,
        scope: &Scope,
        company_id: i64,
        active_only: bool,
    ) -> Result<Vec<TaxCode>> {
        let client = self.db.get().await?;
        authorize(&**client, scope, company_id, VIEW).await?;
        tax_codes::list(&**client, scope.tenant(), company_id, active_only).await
    }

    /// Adds a tax code: `code` (unique in the company, fixed for good), `name`
    /// and `rate`, a percentage from 0 to 100 with up to four decimal places.
    /// Capability [`CONFIGURE`].
    pub async fn create_tax_code(
        &self,
        scope: &Scope,
        company_id: i64,
        tax_code: &NewTaxCode,
    ) -> Result<Outcome<TaxCode>> {
        let mut client = self.db.get().await?;
        let Some(mut tx) = self
            .begin(
                &mut client,
                scope,
                company_id,
                CONFIGURE,
                "supply_chain.purchasing.tax_code.create",
            )
            .await?
        else {
            return Ok(Outcome::CompanyArchived);
        };
        let outcome = tax_codes::create(&mut tx, scope.tenant(), company_id, tax_code).await?;
        finish(tx, &outcome).await?;
        Ok(outcome)
    }

    /// Bilimbi `write/4`: the capability for the target company, then the
    /// company row locked for writing inside the mutation transaction. `None`
    /// is an archived company, already rolled back.
    async fn begin<'c>(
        &self,
        client: &'c mut Client,
        scope: &Scope,
        company_id: i64,
        capability: &str,
        action: &str,
    ) -> Result<Option<MutationTransaction<'c>>> {
        authorize(&*client, scope, company_id, capability).await?;
        let tx = self
            .mutations
            .begin(client, scope.mutation_context(action))
            .await?;
        match organisation::require_writable(tx.sql(), scope, company_id, true).await {
            Ok(_) => Ok(Some(tx)),
            Err(WriteError::Archived) => {
                tx.rollback().await?;
                Ok(None)
            }
            Err(WriteError::NotFound) => Err(Error::NotFound),
            Err(WriteError::Access(error)) => Err(error),
            Err(WriteError::InUse | WriteError::Stale | WriteError::Invalid(_)) => {
                Err(Error::Internal)
            }
        }
    }
}

/// Bilimbi `authorize/3` through Core Company's `authorize_company_target`.
async fn authorize(
    client: &impl GenericClient,
    scope: &Scope,
    company_id: i64,
    capability: &str,
) -> Result<()> {
    organisation::authorize_target(client, scope, company_id, capability)
        .await
        .map(|_| ())
}

async fn finish<T>(tx: MutationTransaction<'_>, outcome: &Outcome<T>) -> Result<()> {
    match outcome {
        Outcome::Saved(_) => tx.commit().await,
        Outcome::Refused(_) | Outcome::CompanyArchived => tx.rollback().await,
    }
}
