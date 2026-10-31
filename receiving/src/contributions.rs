//! Bilimbi Receiving `Contributions`: the descriptor with its facade and
//! pages services, the capability vocabulary, the three page routes and the
//! Goods receipts menu item under Purchasing's Supply chain root.
use crate::{Receiving, MODULE, SERVICE};
use bilibi_authz::{CapabilityDefinition as C, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{Db, MutationBoundary};
use bilibi_events::{EventType, Events, EVENTS};
use bilibi_menu::{MenuItem, Requirement};
use bilibi_module_registry::{
    contributions::{Access, Method, PageWrites, Pending, Route, PAGE_WRITES, ROUTES},
    Contribution, Layer, Module, Provenance, Service,
};
use bilibi_supply_chain_purchasing::Purchasing;
use bilibi_tenancy::Scope;
use bilibi_ui::page::PageService;
use std::sync::Arc;

pub const VIEW: &str = "supply-chain.receiving.view";
pub const CONFIGURE: &str = "supply-chain.receiving.configuration.manage";
pub const MANAGE_RECEIPTS: &str = "supply-chain.receiving.receipts.manage";
pub const POST_RECEIPTS: &str = "supply-chain.receiving.receipts.post-receipt";
pub const CAPABILITIES: [&str; 4] = [VIEW, CONFIGURE, MANAGE_RECEIPTS, POST_RECEIPTS];
pub const GOODS_RECEIVED: &str = "supply_chain/receiving.goods_received";
pub const RECEIPT_REVERSED: &str = "supply_chain/receiving.receipt_reversed";

macro_rules! provenance {
    () => {
        Provenance {
            module: MODULE,
            source: file!(),
            line: line!(),
        }
    };
}
inventory::submit! {
    Module {
        id: MODULE, layer: Layer::Domains,
        dependencies: &["base/authz", "base/database", "base/datetime", "base/events", "base/locale", "base/menu", "base/tenancy", "base/ui", "core/company", "supply_chain/purchasing"],
        migrations: &[("supply-chain-receiving-001-receipts", include_str!("../migrations/001-receipts.sql"))],
        services: &[Service { id: SERVICE, factory: |s| Ok(Arc::new(Receiving {
            db: s.get::<Db>("base/database.pool")?.as_ref().clone(),
            mutations: s.get::<MutationBoundary>("base/database.mutations")?,
            purchasing: s.get::<Purchasing>(bilibi_supply_chain_purchasing::SERVICE)?,
            events: s.get::<Events>(bilibi_events::SERVICE)?,
        })) }, Service { id: PAGES, factory: |s| Ok(Arc::new(PageService::<Scope, bilibi_database::Error>(Arc::new(crate::web::Pages::new(
            s.get::<Db>("base/database.pool")?.as_ref().clone(),
            s.get::<Receiving>(SERVICE)?,
            s.get::<Purchasing>(bilibi_supply_chain_purchasing::SERVICE)?,
            s.get::<bilibi_datetime::DisplayPolicy>("base/datetime.display")?,
            s.get::<bilibi_locale::Locales>("base/locale.locales")?,
        ))))) }],
    }
}
inventory::submit! {
    Contribution::new(VOCABULARY, "supply_chain/receiving.vocabulary", provenance!(), &VocabularyContribution {
        domains: &["supply-chain"], verbs: &["post-receipt"],
        capabilities: &[C::tenant(VIEW), C::tenant(CONFIGURE), C::tenant(MANAGE_RECEIPTS), C::tenant(POST_RECEIPTS)],
        roles: &[RoleDefinition {
            code: "tenant_owner", name: "Tenant Owner",
            description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &CAPABILITIES,
        }], system_principals: &[],
    })
}
inventory::submit! { Contribution::new(EVENTS, GOODS_RECEIVED, provenance!(), &EventType { version: 1 }) }
inventory::submit! { Contribution::new(EVENTS, RECEIPT_REVERSED, provenance!(), &EventType { version: 1 }) }

/// The registry service ID of the module's pages.
const PAGES: &str = "supply_chain/receiving.pages";
/// The page paths, Bilimbi's `priv/web_routes.exs` verbatim.
pub(crate) const RECEIPTS_PATH: &str = "/supply-chain/receipts";
pub(crate) const RECEIVE_PATH: &str = "/supply-chain/receive";

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` README,
// "Page writes declaration and check"). The host refuses a live-event write
// from a page service that declares none.
inventory::submit! {
    Contribution::new(PAGE_WRITES, PAGES, provenance!(), &PageWrites {
        service: PAGES, contracts: &[],
        pending: Pending::Every { reason: "Saving, posting, cancelling and reversing goods receipts and the over-receipt tolerance write through the Receiving facade; no contract yet." },
    })
}

inventory::submit! {
    Contribution::new(ROUTES, "supply_chain/receiving.receipts", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
        method: Method::Get, path: RECEIPTS_PATH, service: PAGES, operation: "receipts",
        access: Access::Capability(VIEW), framed: true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "supply_chain/receiving.receive", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
        method: Method::Get, path: RECEIVE_PATH, service: PAGES, operation: "receive",
        access: Access::Capability(VIEW), framed: true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "supply_chain/receiving.receipt", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
        method: Method::Get, path: "/supply-chain/receipts/{id}", service: PAGES, operation: "receipt",
        access: Access::Capability(VIEW), framed: true,
    })
}

// The "supply_chain" root is Purchasing's, a declared dependency.
bilibi_menu::contribute!(
    "supply_chain/receiving",
    MenuItem::page("supply_chain.receipts", "Goods receipts", RECEIPTS_PATH).company_scoped()
        .under("supply_chain")
        .requires(Requirement::Capability(VIEW))
        .order(25)
);
