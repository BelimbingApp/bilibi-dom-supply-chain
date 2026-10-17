//! What Purchasing adds to the shared catalogs (Bilimbi
//! `Bilimbi.SupplyChain.Purchasing.Contributions`): the module descriptor
//! with its migrations and facade service, the capability vocabulary with the
//! `tenant_owner` grants, and the Supply chain menu.
use crate::port::{OrderLinePort, ORDER_LINE};
use crate::{Purchasing, MODULE, SERVICE};
use bilibi_authz::{CapabilityDefinition as C, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{Db, MutationBoundary};
use bilibi_events::{EventType, Events, EVENTS};
use bilibi_menu::{MenuItem, Requirement};
use bilibi_module_registry::{
    contributions::{Access, Method, Route, ROUTES},
    Contribution, Layer, Module, Provenance, Service,
};
use bilibi_ports::{Port, PORTS};
use bilibi_tenancy::Scope;
use bilibi_ui::page::PageService;
use std::sync::Arc;

// The Authz domain is "supply-chain": a capability segment may not carry an
// underscore, while the container ID and table prefix stay "supply_chain".
/// Reads every purchasing page and facade read ask for.
pub const VIEW: &str = "supply-chain.purchasing.view";
/// Maintains suppliers and tax codes.
pub const CONFIGURE: &str = "supply-chain.purchasing.configuration.manage";
/// Creates, submits and cancels purchase requests.
pub const MANAGE_REQUESTS: &str = "supply-chain.purchasing.requests.manage";
/// Approves or rejects a submitted purchase request.
pub const APPROVE_REQUESTS: &str = "supply-chain.purchasing.requests.approve";
/// Creates, submits, sends, amends, closes and cancels purchase orders.
pub const MANAGE_ORDERS: &str = "supply-chain.purchasing.orders.manage";
/// Approves a submitted purchase order or returns it to draft.
pub const APPROVE_ORDERS: &str = "supply-chain.purchasing.orders.approve";
/// Records or reverses goods received against a sent order; Receiving's seam.
pub const RECEIVE_ORDERS: &str = "supply-chain.purchasing.orders.receive";
/// Every Purchasing capability; the Tenant Owner role holds them all.
pub const CAPABILITIES: [&str; 7] = [
    VIEW,
    CONFIGURE,
    MANAGE_REQUESTS,
    APPROVE_REQUESTS,
    MANAGE_ORDERS,
    APPROVE_ORDERS,
    RECEIVE_ORDERS,
];

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
        id: MODULE,
        layer: Layer::Domains,
        dependencies: &["base/agent_api", "base/authz", "base/database", "base/datetime", "base/events", "base/locale", "base/menu", "base/ports", "base/tenancy", "base/ui", "core/company"],
        migrations: &[
            (
                "supply-chain-purchasing-001-suppliers-tax-codes",
                include_str!("../migrations/001-suppliers-tax-codes.sql"),
            ),
            (
                "supply-chain-purchasing-002-requests-orders",
                include_str!("../migrations/002-requests-orders.sql"),
            ),
        ],
        services: &[
            Service {
                id: SERVICE,
                factory: |services| {
                    let events = services.get::<Events>(bilibi_events::SERVICE)?;
                    crate::agent_operations::bind(events.clone())?;
                    Ok(Arc::new(Purchasing::new(
                        services.get::<Db>("base/database.pool")?.as_ref().clone(),
                        services.get::<MutationBoundary>("base/database.mutations")?,
                        events,
                    )))
                },
            },
            Service {
                id: PAGES,
                factory: |services| {
                    let db = services.get::<Db>("base/database.pool")?.as_ref().clone();
                    let purchasing = Purchasing::new(
                        db.clone(),
                        services.get::<MutationBoundary>("base/database.mutations")?,
                        services.get::<Events>(bilibi_events::SERVICE)?,
                    );
                    Ok(Arc::new(PageService::<Scope, bilibi_database::Error>(Arc::new(
                        crate::web::Pages::new(db, purchasing, services.get::<bilibi_datetime::DisplayPolicy>("base/datetime.display")?, services.get::<bilibi_locale::Locales>("base/locale.locales")?),
                    ))))
                },
            },
        ],
    }
}

// Bilibi merges verbs across modules, so `approve` and `receive` are declared
// here beside Base's own; only a duplicate capability key would fail boot.
inventory::submit! {
    Contribution::new(VOCABULARY, "supply_chain/purchasing.vocabulary", provenance!(), &VocabularyContribution {
        domains: &["supply-chain"],
        verbs: &["approve", "receive"],
        capabilities: &[
            C::tenant(VIEW), C::tenant(CONFIGURE), C::tenant(MANAGE_REQUESTS), C::tenant(APPROVE_REQUESTS),
            C::tenant(MANAGE_ORDERS), C::tenant(APPROVE_ORDERS), C::tenant(RECEIVE_ORDERS),
        ],
        roles: &[RoleDefinition {
            code: "tenant_owner",
            name: "Tenant Owner",
            description: "Contributed system role; reconciled explicitly",
            grant_all: false,
            capabilities: &CAPABILITIES,
        }],
        system_principals: &[],
    })
}

// Bilimbi's `OrderSource` seam: a module that may run without Supply Chain
// reads one order line through `base/ports` (contract in README.md).
inventory::submit! {
    Contribution::new(PORTS, ORDER_LINE, provenance!(), &Port { provider: &OrderLinePort })
}

inventory::submit! {
    Contribution::new(EVENTS, crate::order_events::APPROVED, provenance!(), &EventType { version: 1 })
}
inventory::submit! {
    Contribution::new(EVENTS, crate::order_events::AMENDED, provenance!(), &EventType { version: 1 })
}
inventory::submit! {
    Contribution::new(EVENTS, crate::order_events::STATUS_CHANGED, provenance!(), &EventType { version: 1 })
}

/// The registry service ID of the module's pages.
const PAGES: &str = "supply_chain/purchasing.pages";
/// The page paths, Bilimbi's `priv/web_routes.exs` verbatim.
pub(crate) const SUPPLIERS_PATH: &str = "/supply-chain/suppliers";
pub(crate) const ORDERS_PATH: &str = "/supply-chain/purchase-orders";
pub(crate) const REQUESTS_PATH: &str = "/supply-chain/purchase-requests";

inventory::submit! {
    Contribution::new(ROUTES, "supply_chain/purchasing.suppliers", provenance!(), &Route {
        method: Method::Get, path: SUPPLIERS_PATH, service: PAGES, operation: "suppliers",
        access: Access::Capability(VIEW), framed: true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "supply_chain/purchasing.requests", provenance!(), &Route {
        method: Method::Get, path: REQUESTS_PATH, service: PAGES, operation: "requests",
        access: Access::Capability(VIEW), framed: true,
    })
}

// The Supply chain root stays hidden until an account may see a child.
bilibi_menu::contribute!(
    "supply_chain/purchasing",
    MenuItem::container("supply_chain", "Supply chain")
        .icon("clipboard-document-list")
        .order(55)
);
bilibi_menu::contribute!(
    "supply_chain/purchasing",
    MenuItem::page(
        "supply_chain.purchase_requests",
        "Purchase requests",
        REQUESTS_PATH
    )
    .under("supply_chain")
    .requires(Requirement::Capability(VIEW))
    .order(10)
);
bilibi_menu::contribute!(
    "supply_chain/purchasing",
    MenuItem::page(
        "supply_chain.purchase_orders",
        "Purchase orders",
        "/supply-chain/purchase-orders"
    )
    .under("supply_chain")
    .requires(Requirement::Capability(VIEW))
    .order(20)
);
bilibi_menu::contribute!(
    "supply_chain/purchasing",
    MenuItem::page("supply_chain.suppliers", "Suppliers", SUPPLIERS_PATH)
        .under("supply_chain")
        .requires(Requirement::Capability(VIEW))
        .order(30)
);

inventory::submit! {
    Contribution::new(ROUTES, "supply_chain/purchasing.orders", provenance!(), &Route {
        method: Method::Get, path: ORDERS_PATH, service: PAGES, operation: "orders",
        access: Access::Capability(VIEW), framed: true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "supply_chain/purchasing.order", provenance!(), &Route {
        method: Method::Get, path: "/supply-chain/purchase-orders/{id}", service: PAGES, operation: "order",
        access: Access::Capability(VIEW), framed: true,
    })
}
