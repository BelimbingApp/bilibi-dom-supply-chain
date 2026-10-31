//! Receiving (`supply_chain/receiving`), ported from Bilimbi's facade.
//! One authorized facade over private SQL, validation and lifecycle rules.
//! Writes serialize on the writable company and atomically commit receipt,
//! Purchasing quantities, audit and durable domain events. See README.md.
mod contributions;
mod lifecycle;
mod records;
mod types;
mod validate;
mod web;

pub use contributions::{
    CAPABILITIES, CONFIGURE, GOODS_RECEIVED, MANAGE_RECEIPTS, POST_RECEIPTS, RECEIPT_REVERSED, VIEW,
};
pub use types::*;

use bilibi_company::WriteError;
use bilibi_database::{Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_events::Events;
use bilibi_supply_chain_purchasing::Purchasing;
use bilibi_tenancy::Scope;
use std::sync::Arc;
use tokio_postgres::{Client, GenericClient};

pub const MODULE: &str = "supply_chain/receiving";
pub const SERVICE: &str = "supply_chain/receiving.receiving";
pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 500;

macro_rules! write {
    ($self:ident, $scope:ident, $company:ident, $cap:ident, $action:literal, |$tx:ident| $body:expr) => {{
        let mut client = $self.db.get().await?;
        let Some(mut $tx) = $self
            .begin(&mut client, $scope, $company, $cap, $action)
            .await?
        else {
            return Ok(Outcome::CompanyArchived);
        };
        let result = $body.await;
        // Roll back errors as well as business refusals. In particular,
        // Events preflight errors need not poison a transaction themselves.
        match result {
            Ok(Outcome::Saved(value)) => {
                $tx.commit().await?;
                Ok(Outcome::Saved(value))
            }
            Ok(other) => {
                $tx.rollback().await?;
                Ok(other)
            }
            Err(error) => {
                $tx.rollback().await?;
                Err(error)
            }
        }
    }};
}

pub struct Receiving {
    db: Db,
    mutations: Arc<MutationBoundary>,
    purchasing: Arc<Purchasing>,
    events: Arc<Events>,
}
impl Receiving {
    /// Current append-only setting, or zero with absent setter/time.
    pub async fn get_settings(&self, scope: &Scope, company_id: i64) -> Result<Settings> {
        let c = self.db.get().await?;
        authorize(&**c, scope, company_id, VIEW).await?;
        records::settings(&**c, scope.tenant(), company_id).await
    }

    /// Append a tolerance version (0..100 percent, at most four places).
    pub async fn configure(
        &self,
        scope: &Scope,
        company_id: i64,
        percent: &str,
    ) -> Result<Outcome<Settings>> {
        write!(
            self,
            scope,
            company_id,
            CONFIGURE,
            "supply_chain.receiving.configure",
            |tx| lifecycle::configure(&mut tx, scope, company_id, percent)
        )
    }

    /// Newest receipt number first, lines included, history empty.
    pub async fn list_receipts(
        &self,
        scope: &Scope,
        company_id: i64,
        query: ReceiptQuery,
    ) -> Result<Vec<Receipt>> {
        let c = self.db.get().await?;
        authorize(&**c, scope, company_id, VIEW).await?;
        limit(query.limit)?;
        if query.order_id.is_some_and(|id| id < 1) {
            return Err(Error::Invalid);
        }
        records::list(&**c, scope.tenant(), company_id, query).await
    }

    pub async fn get_receipt(
        &self,
        scope: &Scope,
        company_id: i64,
        receipt_id: i64,
    ) -> Result<Option<Receipt>> {
        let c = self.db.get().await?;
        authorize(&**c, scope, company_id, VIEW).await?;
        records::get(&**c, scope.tenant(), company_id, receipt_id, true).await
    }

    pub async fn find_receipt_by_number(
        &self,
        scope: &Scope,
        company_id: i64,
        number: i64,
    ) -> Result<Option<Receipt>> {
        let c = self.db.get().await?;
        authorize(&**c, scope, company_id, VIEW).await?;
        let row = c.query_opt("SELECT id FROM supply_chain_receiving_receipts WHERE tenant_id=$1 AND company_id=$2 AND receipt_number=$3", &[&scope.tenant(), &company_id, &number]).await?;
        match row {
            Some(row) => {
                records::get(&**c, scope.tenant(), company_id, row.try_get(0)?, true).await
            }
            None => Ok(None),
        }
    }

    pub async fn create_receipt(
        &self,
        scope: &Scope,
        company_id: i64,
        input: &NewReceipt,
    ) -> Result<Outcome<Receipt>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_RECEIPTS,
            "supply_chain.receiving.receipt.create",
            |tx| lifecycle::create(self, &mut tx, scope, company_id, input)
        )
    }

    pub async fn update_receipt(
        &self,
        scope: &Scope,
        company_id: i64,
        receipt_id: i64,
        input: &ReceiptChanges,
    ) -> Result<Outcome<Receipt>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_RECEIPTS,
            "supply_chain.receiving.receipt.update",
            |tx| lifecycle::update(self, &mut tx, scope, company_id, receipt_id, input)
        )
    }

    pub async fn cancel_receipt(
        &self,
        scope: &Scope,
        company_id: i64,
        receipt_id: i64,
        reason: &str,
    ) -> Result<Outcome<Receipt>> {
        write!(
            self,
            scope,
            company_id,
            MANAGE_RECEIPTS,
            "supply_chain.receiving.receipt.cancel",
            |tx| lifecycle::cancel(&mut tx, scope, company_id, receipt_id, reason)
        )
    }

    pub async fn post_receipt(
        &self,
        scope: &Scope,
        company_id: i64,
        receipt_id: i64,
    ) -> Result<Outcome<Receipt>> {
        write!(
            self,
            scope,
            company_id,
            POST_RECEIPTS,
            "supply_chain.receiving.receipt.post",
            |tx| lifecycle::post(self, &mut tx, scope, company_id, receipt_id)
        )
    }

    pub async fn reverse_receipt(
        &self,
        scope: &Scope,
        company_id: i64,
        receipt_id: i64,
        input: &Reversal,
    ) -> Result<Outcome<Receipt>> {
        write!(
            self,
            scope,
            company_id,
            POST_RECEIPTS,
            "supply_chain.receiving.receipt.reverse",
            |tx| lifecycle::reverse(self, &mut tx, scope, company_id, receipt_id, input)
        )
    }

    /// Net posted accepted/rejected figures for current or past order lines.
    /// Each contributing receipt retains positive-direction quantities.
    pub async fn order_receipts(
        &self,
        scope: &Scope,
        company_id: i64,
        order_id: i64,
    ) -> Result<Vec<OrderLineReceipts>> {
        let c = self.db.get().await?;
        authorize(&**c, scope, company_id, VIEW).await?;
        records::summaries(&**c, scope.tenant(), company_id, Some(order_id), None).await
    }

    pub async fn order_line_receipts(
        &self,
        scope: &Scope,
        company_id: i64,
        line_id: i64,
    ) -> Result<OrderLineReceipts> {
        let c = self.db.get().await?;
        authorize(&**c, scope, company_id, VIEW).await?;
        Ok(
            records::summaries(&**c, scope.tenant(), company_id, None, Some(line_id))
                .await?
                .pop()
                .unwrap_or_else(|| OrderLineReceipts::empty(line_id)),
        )
    }

    /// Cursor is the Receiving posting-history ID, not receipt/outbox ID.
    pub async fn list_postings(
        &self,
        scope: &Scope,
        company_id: i64,
        query: PostingQuery,
    ) -> Result<Vec<Posting>> {
        let c = self.db.get().await?;
        authorize(&**c, scope, company_id, VIEW).await?;
        limit(query.limit)?;
        if query.after_posting_id.is_some_and(|id| id < 1) {
            return Err(Error::Invalid);
        }
        records::postings(&**c, scope.tenant(), company_id, query).await
    }

    async fn begin<'a>(
        &self,
        client: &'a mut Client,
        scope: &Scope,
        company_id: i64,
        capability: &str,
        action: &str,
    ) -> Result<Option<MutationTransaction<'a>>> {
        let tx = self
            .mutations
            .begin(client, scope.mutation_context(action))
            .await?;
        authorize(tx.sql(), scope, company_id, capability).await?;
        match bilibi_company::require_writable(tx.sql(), scope, company_id, true).await {
            Ok(_) => Ok(Some(tx)),
            Err(WriteError::Archived) => {
                tx.rollback().await?;
                Ok(None)
            }
            Err(WriteError::NotFound) => Err(Error::NotFound),
            Err(WriteError::Access(e)) => Err(e),
            Err(_) => Err(Error::Internal),
        }
    }
}

async fn authorize(
    c: &(impl GenericClient + Sync),
    scope: &Scope,
    company_id: i64,
    cap: &str,
) -> Result<()> {
    bilibi_company::authorize_work_target(c, scope, company_id, cap)
        .await
        .map(|_| ())
}
fn limit(value: i64) -> Result<()> {
    if (1..=MAX_LIMIT).contains(&value) {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}
