//! The Receiving pages (Bilimbi `Receiving.Web`): Goods receipts
//! (`ReceiptsLive`), Receive goods (`ReceiveLive`) and one receipt
//! (`ReceiptLive`), live pages over the [`Receiving`] facade and, for the
//! orders a receipt is taken against, Purchasing's facade. Each page reads the
//! signed-in account's own company from its sealed scope, checks the
//! capability of every write-shaped event again before acting, and the facade
//! checks it once more inside the write. A refusal or a lost grant is a flash,
//! never a page-clearing error; `Refusal::text` and [`failure`] hold Bilimbi's
//! sentences (`Web.Refusals`), so pages do not word refusals themselves.
mod lists;
mod receipt;
mod receipts;
mod receive;

use crate::{Outcome, Receiving};
use bilibi_database::{Db, Error, Result};
use bilibi_supply_chain_purchasing::Purchasing;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveState, MountFuture, Page, PageBody, PageFuture, PageRequest},
    Badge, BadgeKind, FlashMap, Html, StatusKind, NO_FLASH,
};
use std::{collections::BTreeSet, sync::Arc};

pub(crate) struct Core {
    db: Db,
    receiving: Arc<Receiving>,
    purchasing: Arc<Purchasing>,
    display: Arc<bilibi_datetime::DisplayPolicy>,
    locales: Arc<bilibi_locale::Locales>,
}

pub(crate) struct Pages(Arc<Core>);
impl Pages {
    pub(crate) fn new(
        db: Db,
        receiving: Arc<Receiving>,
        purchasing: Arc<Purchasing>,
        display: Arc<bilibi_datetime::DisplayPolicy>,
        locales: Arc<bilibi_locale::Locales>,
    ) -> Self {
        Self(Arc::new(Core {
            db,
            receiving,
            purchasing,
            display,
            locales,
        }))
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "receipts" => Some("Goods receipts"),
            "receive" => Some("Receive goods"),
            "receipt" => Some("Goods receipt"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut title = None;
            let html = match request.operation {
                "receipts" => {
                    let mut page =
                        receipts::Receipts::new(self.0.clone(), request.query, request.framed)?;
                    page.load(scope).await?;
                    page.render()?
                }
                "receive" => {
                    let mut page = receive::Receive::new(self.0.clone(), request.query);
                    page.load(scope).await?;
                    page.render()?
                }
                "receipt" => {
                    let mut page =
                        receipt::ReceiptPage::new(self.0.clone(), request.path, request.query)?;
                    page.load(scope, true).await?;
                    title = Some(page.title());
                    page.render()?
                }
                _ => return Err(Error::NotFound),
            };
            Ok(PageBody {
                html,
                live: true,
                title,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                "receipts" => Box::new(receipts::Receipts::new(
                    self.0.clone(),
                    request.query,
                    request.framed,
                )?),
                "receive" => Box::new(receive::Receive::new(self.0.clone(), request.query)),
                "receipt" => Box::new(receipt::ReceiptPage::new(
                    self.0.clone(),
                    request.path,
                    request.query,
                )?),
                _ => return Ok(None),
            };
            Ok(Some(state))
        })
    }
}

/// The company the pages work in: the signed-in account's own (Bilimbi
/// `current_scope.user["company_id"]`). An account with none names no
/// company, which the facade answers as not found.
fn company_id(scope: &Scope) -> i64 {
    scope.actor().organisation().map_or(0, |id| id.get())
}

/// The capabilities the scope holds now (Bilimbi `Authz.can/2`), read again
/// for every write-shaped event.
async fn capabilities(core: &Core, scope: &Scope) -> Result<BTreeSet<&'static str>> {
    let client = core.db.get().await?;
    bilibi_authz::effective_capabilities(&**client, scope).await
}

/// A read the page shows as empty when the grant or the company went away
/// since mount, as Bilimbi's `load/1` assigns `[]` on any error.
fn or_empty<T>(result: Result<Vec<T>>) -> Result<Vec<T>> {
    match result {
        Err(Error::Forbidden | Error::NotFound) => Ok(Vec::new()),
        other => other,
    }
}

/// The sentence for a write the facade did not run (Bilimbi
/// `Refusals.text/1` for `:unauthorized`, `:company_not_found`, and the
/// catch-all for an option the facade refused); any other error is a fault.
fn failure(error: Error) -> Result<String> {
    match error {
        Error::Forbidden => Ok("You no longer have permission to do that for this company.".into()),
        Error::NotFound => Ok("That company is no longer available.".into()),
        Error::Invalid => Ok("The action could not be completed.".into()),
        error => Err(error),
    }
}

/// How a facade write ended, as the page flashes it: the record, or the
/// error sentence.
fn outcome<T>(result: Result<Outcome<T>>) -> Result<std::result::Result<T, String>> {
    Ok(match result {
        Ok(Outcome::Saved(record)) => Ok(record),
        Ok(Outcome::Refused(refusal)) => Err(refusal.text()),
        Ok(Outcome::CompanyArchived) => {
            Err("This company is archived; nothing in it can change.".into())
        }
        Err(error) => Err(failure(error)?),
    })
}

/// The one message a page shows, and the flash-group dismissal that clears it.
#[derive(Default)]
struct Flash(Option<(StatusKind, String)>);
impl Flash {
    fn success(&mut self, message: impl Into<String>) {
        self.0 = Some((StatusKind::Success, message.into()));
    }
    fn error(&mut self, message: impl Into<String>) {
        self.0 = Some((StatusKind::Error, message.into()));
    }
    fn clear(&mut self, key: Option<&str>) {
        if self
            .0
            .as_ref()
            .is_some_and(|(kind, _)| Some(kind.name()) == key)
        {
            self.0 = None;
        }
    }
    fn map(&self) -> FlashMap<'_> {
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.0 {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        flash
    }
}

fn render(fragment: &impl askama::Template) -> Result<Html> {
    Html::render(fragment).map_err(|_| Error::Internal)
}

/// A string value of a live event payload.
fn text<'a>(payload: &'a serde_json::Value, key: &str) -> &'a str {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

/// A status or kind as a person reads it (Bilimbi `Forms.label/1`).
fn label(value: &str) -> String {
    let words = value.replace('_', " ");
    let mut chars = words.chars();
    chars.next().map_or(String::new(), |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// A receipt status badge (Bilimbi `Forms.status_kind/1`).
fn badge(status: crate::ReceiptStatus) -> Result<Html> {
    render(&Badge::status(
        match status {
            crate::ReceiptStatus::Posted => BadgeKind::Success,
            crate::ReceiptStatus::Cancelled => BadgeKind::Danger,
            crate::ReceiptStatus::Draft => BadgeKind::Neutral,
        },
        &label(status.as_str()),
    ))
}

/// A receipt's own page.
fn address(id: i64) -> String {
    format!("{}/{id}", crate::contributions::RECEIPTS_PATH)
}
