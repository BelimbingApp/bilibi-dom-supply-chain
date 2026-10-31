//! The Purchasing pages (Bilimbi `Purchasing.Web`): Suppliers
//! (`SuppliersLive`), Purchase requests (`RequestsLive`), Purchase orders
//! (`OrdersLive`) and order detail (`OrderLive`), live pages over
//! the [`Purchasing`] facade. Each page reads the signed-in account's own
//! company from its sealed scope, checks the capability of every
//! write-shaped event again before acting, and the facade checks it once
//! more inside the write. A refusal or a lost grant is a flash, never a
//! page-clearing error; `Refusal::text` and [`failure`] hold Bilimbi's
//! sentences (`Web.Refusals`), so pages do not word refusals themselves.
mod lists;
mod order;
mod orders;
mod requests;
mod suppliers;

use crate::{Outcome, Purchasing};
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveState, MountFuture, Page, PageBody, PageFuture, PageRequest},
    FlashMap, Html, StatusKind, NO_FLASH,
};
use std::{collections::BTreeSet, sync::Arc};

pub(crate) struct Core {
    db: Db,
    purchasing: Purchasing,
    display: Arc<bilibi_datetime::DisplayPolicy>,
    locales: Arc<bilibi_locale::Locales>,
}

pub(crate) struct Pages(Arc<Core>);
impl Pages {
    pub(crate) fn new(
        db: Db,
        purchasing: Purchasing,
        display: Arc<bilibi_datetime::DisplayPolicy>,
        locales: Arc<bilibi_locale::Locales>,
    ) -> Self {
        Self(Arc::new(Core {
            db,
            purchasing,
            display,
            locales,
        }))
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "suppliers" => Some("Suppliers"),
            "requests" => Some("Purchase requests"),
            "orders" => Some("Purchase orders"),
            "order" => Some("Purchase order"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut title = None;
            let html = match request.operation {
                "orders" => {
                    let mut page =
                        orders::Orders::new(self.0.clone(), request.query, request.framed)?;
                    page.load(scope).await?;
                    page.render()?
                }
                "order" => {
                    let mut page =
                        order::OrderPage::new(self.0.clone(), request.path, request.query)?;
                    page.load(scope, true).await?;
                    title = Some(page.title());
                    page.render()?
                }
                "suppliers" => {
                    let mut page = suppliers::Suppliers::new(self.0.clone(), request.query)?;
                    page.load(scope).await?;
                    page.render()?
                }
                "requests" => {
                    let mut page = requests::Requests::new(self.0.clone(), request.query)?;
                    page.load(scope).await?;
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
                "orders" => Box::new(orders::Orders::new(
                    self.0.clone(),
                    request.query,
                    request.framed,
                )?),
                "order" => Box::new(order::OrderPage::new(
                    self.0.clone(),
                    request.path,
                    request.query,
                )?),
                "suppliers" => Box::new(suppliers::Suppliers::new(self.0.clone(), request.query)?),
                "requests" => Box::new(requests::Requests::new(self.0.clone(), request.query)?),
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
    scope.current_company().map_or(0, |id| id.get())
}

/// Whether the scope holds `capability` now (Bilimbi `Authz.can/2`).
async fn can(core: &Core, scope: &Scope, capability: &str) -> Result<bool> {
    Ok(capabilities(core, scope).await?.contains(capability))
}

/// The capabilities the scope holds now, for a page that asks after several.
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
/// `Refusals.text/1` for `:unauthorized`, `:company_not_found`); any other
/// error is a fault, not a refusal.
fn failure(error: Error) -> Result<String> {
    match error {
        Error::Forbidden => Ok("You no longer have permission to do that for this company.".into()),
        Error::NotFound => Ok("That company is no longer available.".into()),
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

/// A positive record ID from a payload string, or `None` (Bilimbi
/// `Forms.integer/1`).
fn id(payload: &serde_json::Value, key: &str) -> Option<i64> {
    bilibi_ui::params::positive_id(Some(text(payload, key)))
}
