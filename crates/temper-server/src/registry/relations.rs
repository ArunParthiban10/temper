//! Relation graph construction and webhook route indexing.

use std::collections::BTreeMap;

use temper_spec::automaton::Webhook;
use temper_spec::cross_invariant::{CrossInvariantSpec, DeletePolicy};
use temper_spec::csdl::CsdlDocument;

use super::types::{EntitySpec, RelationEdge, RelationGraph};

/// Build webhook route index from parsed entity specs.
pub(super) fn build_webhook_routes(
    entities: &BTreeMap<String, EntitySpec>,
) -> BTreeMap<String, (String, Webhook)> {
    let mut routes = BTreeMap::new();
    for (entity_type, spec) in entities {
        for wh in &spec.automaton.webhooks {
            routes.insert(wh.path.clone(), (entity_type.clone(), wh.clone()));
        }
    }
    routes
}

/// Build a relation graph from the CSDL and optional cross-invariant overrides.
pub(super) fn build_relation_graph(
    csdl: &CsdlDocument,
    cross_invariants: Option<&CrossInvariantSpec>,
) -> RelationGraph {
    let mut overrides = BTreeMap::<(String, String), DeletePolicy>::new();
    let default_policy = cross_invariants
        .map(|spec| {
            for ov in &spec.relation_overrides {
                overrides.insert(
                    (ov.from_entity.clone(), ov.navigation_property.clone()),
                    ov.delete_policy,
                );
            }
            spec.default_delete_policy
        })
        .unwrap_or(DeletePolicy::Restrict);

    let mut graph = RelationGraph::default();
    for schema in &csdl.schemas {
        for et in &schema.entity_types {
            for nav in &et.navigation_properties {
                let target = nav_target_entity(&nav.type_name);
                for rc in &nav.referential_constraints {
                    let delete_policy = overrides
                        .get(&(et.name.clone(), nav.name.clone()))
                        .copied()
                        .unwrap_or(default_policy);
                    let edge = RelationEdge {
                        from_entity: et.name.clone(),
                        navigation_property: nav.name.clone(),
                        to_entity: target.clone(),
                        source_field: rc.property.clone(),
                        target_field: rc.referenced_property.clone(),
                        nullable: nav.nullable,
                        delete_policy,
                    };
                    graph
                        .outgoing
                        .entry(et.name.clone())
                        .or_default()
                        .push(edge.clone());
                    graph.incoming.entry(target.clone()).or_default().push(edge);
                }
            }
        }
    }
    graph
}

/// Extract the target entity type name from a CSDL navigation type string.
fn nav_target_entity(type_name: &str) -> String {
    let raw = type_name.trim();
    let inner = if raw.starts_with("Collection(") && raw.ends_with(')') {
        &raw[11..raw.len() - 1]
    } else {
        raw
    };
    inner.rsplit('.').next().unwrap_or(inner).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_target_simple_type() {
        assert_eq!(nav_target_entity("Order"), "Order");
    }

    #[test]
    fn nav_target_qualified_type() {
        assert_eq!(nav_target_entity("MyNamespace.Order"), "Order");
    }

    #[test]
    fn nav_target_collection_type() {
        assert_eq!(
            nav_target_entity("Collection(MyNamespace.OrderItem)"),
            "OrderItem"
        );
    }

    #[test]
    fn nav_target_collection_simple() {
        assert_eq!(nav_target_entity("Collection(Item)"), "Item");
    }

    #[test]
    fn nav_target_whitespace_trimmed() {
        assert_eq!(nav_target_entity("  Order  "), "Order");
    }
}
