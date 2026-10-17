//! Bilimbi Receiving descriptor and capability vocabulary. Routes/menu ship
//! with S6, so S5 does not advertise destinations that do not exist yet.
use crate::{Receiving, MODULE, SERVICE};
use bilibi_authz::{CapabilityDefinition as C, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{Db, MutationBoundary};
use bilibi_events::{EventType, Events, EVENTS};
use bilibi_module_registry::{Contribution, Layer, Module, Provenance, Service};
use bilibi_supply_chain_purchasing::Purchasing;
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
        dependencies: &["base/authz", "base/database", "base/events", "base/tenancy", "core/company", "supply_chain/purchasing"],
        migrations: &[("supply-chain-receiving-001-receipts", include_str!("../migrations/001-receipts.sql"))],
        services: &[Service { id: SERVICE, factory: |s| Ok(Arc::new(Receiving {
            db: s.get::<Db>("base/database.pool")?.as_ref().clone(),
            mutations: s.get::<MutationBoundary>("base/database.mutations")?,
            purchasing: s.get::<Purchasing>(bilibi_supply_chain_purchasing::SERVICE)?,
            events: s.get::<Events>(bilibi_events::SERVICE)?,
        })) }],
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
