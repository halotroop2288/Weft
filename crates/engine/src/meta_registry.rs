//! Engine-side data model for the meta-registry.
//!
//! This is the engine-side metadata model for replacing the ABI's fixed registry-kind list.
//! The loader collects declarations before ordinary initialization; per-registry entry loading
//! and dynamic registry-backed entry tables are separate follow-up steps.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use weft_abi::ModId;
pub use weft_abi::RegistryId;

/// A registry is declared by exactly one mod.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryDeclaration {
    pub name: String,
    pub owner: ModId,
    /// Engine-private specialized builder, when this is a core-owned built-in registry.
    pub runtime_kind: Option<u32>,
}

/// A frozen registry descriptor, including every mod allowed to contribute entries to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryDescriptor {
    pub id: RegistryId,
    pub name: String,
    pub owner: ModId,
    /// Engine-private specialized builder; never exposed through the mod ABI.
    pub runtime_kind: Option<u32>,
    pub contributors: Vec<ModId>,
}

/// Errors found while constructing the meta-registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryError {
    InvalidName(String),
    DuplicateDeclaration(String),
    UndeclaredRegistry(String),
    UnknownOrderEntry(String),
    DuplicateOrderEntry(String),
    UnknownDependency { registry: String, dependency: String },
    DependencyCycle(Vec<String>),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName(name) => write!(f, "invalid registry identifier {name:?}; expected namespace:path"),
            Self::DuplicateDeclaration(name) => write!(f, "registry {name} was declared more than once"),
            Self::UndeclaredRegistry(name) => write!(f, "cannot contribute to undeclared registry {name}"),
            Self::UnknownOrderEntry(name) => write!(f, "registry order names unknown registry {name}"),
            Self::DuplicateOrderEntry(name) => write!(f, "registry order lists {name} more than once"),
            Self::UnknownDependency { registry, dependency } => {
                write!(f, "registry {registry} has an ordering constraint referencing unknown registry {dependency}")
            }
            Self::DependencyCycle(registries) => write!(f, "registry ordering contains a cycle: {}", registries.join(" -> ")),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Mutable declaration/contribution set built during the meta-registry phase.
///
/// Declaration ownership and entry contribution are deliberately separate: declaring a registry
/// automatically permits its owner to contribute, but any other loaded mod can be added too.
#[derive(Default)]
pub struct MetaRegistry {
    declarations: BTreeMap<String, RegistryDeclaration>,
    contributors: BTreeMap<String, BTreeSet<u32>>,
    before: BTreeMap<String, BTreeSet<String>>,
    after: BTreeMap<String, BTreeSet<String>>,
}

impl MetaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a registry and return the builder for its ordering constraints.
    ///
    /// Names are fully qualified. Calling this creates a new registry entry; it does not
    /// retrieve or mutate a registry declared by another mod.
    pub fn registry<'a>(&'a mut self, name: impl Into<String>, owner: ModId) -> Result<RegistryBuilder<'a>, RegistryError> {
        let name = name.into();
        self.declare_with_runtime_kind(name.clone(), owner, None)?;
        Ok(RegistryBuilder { meta: self, name })
    }

    /// Add an ordering constraint to a registry declaration.
    pub fn order_before(&mut self, registry: &str, other: &str) {
        self.before
            .entry(registry.to_string())
            .or_default()
            .insert(other.to_string());
    }

    /// Add an ordering constraint to a registry declaration.
    pub fn order_after(&mut self, registry: &str, other: &str) {
        self.after
            .entry(registry.to_string())
            .or_default()
            .insert(other.to_string());
    }

    /// Engine-only declaration for registries backed by specialized runtime builders.
    pub fn declare(&mut self, name: impl Into<String>, owner: ModId) -> Result<(), RegistryError> {
        self.declare_with_runtime_kind(name.into(), owner, None)
    }

    /// Declare an engine-owned registry backed by a specialized runtime builder.
    ///
    /// The numeric builder slot is engine-private and is not part of the registry ABI.
    pub fn declare_core(&mut self, name: impl Into<String>, runtime_kind: u32) -> Result<(), RegistryError> {
        self.declare_with_runtime_kind(name.into(), ModId(u32::MAX), Some(runtime_kind))
    }

    fn declare_with_runtime_kind(&mut self, name: String, owner: ModId, runtime_kind: Option<u32>) -> Result<(), RegistryError> {
        if !valid_name(&name) {
            return Err(RegistryError::InvalidName(name));
        }
        if self.declarations.contains_key(&name) {
            return Err(RegistryError::DuplicateDeclaration(name));
        }

        self.declarations
            .insert(name.clone(), RegistryDeclaration { name: name.clone(), owner, runtime_kind });
        self.contributors.entry(name).or_default().insert(owner.0);
        Ok(())
    }

    /// Allow a mod to contribute entries to a registry declared by any loaded mod.
    ///
    /// This does not transfer ownership. Repeated contributions by the same mod are idempotent.
    pub fn contribute(&mut self, registry: &str, contributor: ModId) -> Result<(), RegistryError> {
        if !self.declarations.contains_key(registry) {
            return Err(RegistryError::UndeclaredRegistry(registry.to_string()));
        }
        self.contributors
            .entry(registry.to_string())
            .or_default()
            .insert(contributor.0);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.declarations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }

    /// Assign stable IDs and resolve phases using a deterministic topological sort.
    /// IDs follow sorted names; independent phases are selected lexicographically.
    /// The explicit-order argument is retained temporarily for source compatibility and ignored.
    pub fn freeze(self, _explicit_order: &[String], mod_order: &[ModId]) -> Result<FrozenMetaRegistry, Vec<RegistryError>> {
        let ids: BTreeMap<String, RegistryId> = self
            .declarations
            .keys()
            .enumerate()
            .map(|(i, name)| (name.clone(), RegistryId(i as u32)))
            .collect();
        let mut errors = Vec::new();
        let mut edges: BTreeMap<String, BTreeSet<String>> = self
            .declarations
            .keys()
            .map(|name| (name.clone(), BTreeSet::new()))
            .collect();

        for (registry, dependencies) in &self.before {
            for dependency in dependencies {
                if !self.declarations.contains_key(dependency) {
                    errors.push(RegistryError::UnknownDependency { registry: registry.clone(), dependency: dependency.clone() });
                } else {
                    edges.get_mut(registry).unwrap().insert(dependency.clone());
                }
            }
        }
        for (registry, dependencies) in &self.after {
            for dependency in dependencies {
                if !self.declarations.contains_key(dependency) {
                    errors.push(RegistryError::UnknownDependency { registry: registry.clone(), dependency: dependency.clone() });
                } else {
                    edges.get_mut(dependency).unwrap().insert(registry.clone());
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        let mut indegree: BTreeMap<String, usize> = self
            .declarations
            .keys()
            .map(|name| (name.clone(), 0))
            .collect();
        for targets in edges.values() {
            for target in targets {
                *indegree.get_mut(target).unwrap() += 1;
            }
        }
        let mut ready: BTreeSet<String> = indegree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(name, _)| name.clone())
            .collect();
        let mut ordered_names = Vec::with_capacity(self.declarations.len());
        while let Some(name) = ready.pop_first() {
            ordered_names.push(name.clone());
            for target in &edges[&name] {
                let degree = indegree.get_mut(target).unwrap();
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(target.clone());
                }
            }
        }
        if ordered_names.len() != self.declarations.len() {
            let cycle = indegree
                .into_iter()
                .filter(|(_, degree)| *degree > 0)
                .map(|(name, _)| name)
                .collect();
            return Err(vec![RegistryError::DependencyCycle(cycle)]);
        }

        let mut mod_rank = HashMap::with_capacity(mod_order.len());
        for (rank, id) in mod_order.iter().copied().enumerate() {
            mod_rank.entry(id.0).or_insert(rank);
        }
        let mut registries = Vec::with_capacity(self.declarations.len());
        for (name, declaration) in self.declarations {
            let mut contributors: Vec<ModId> = self
                .contributors
                .get(&name)
                .into_iter()
                .flatten()
                .copied()
                .map(ModId)
                .collect();
            contributors.sort_by_key(|id| (mod_rank.get(&id.0).copied().unwrap_or(usize::MAX), id.0));
            registries.push(RegistryDescriptor {
                id: ids[&name],
                name,
                owner: declaration.owner,
                runtime_kind: declaration.runtime_kind,
                contributors,
            });
        }
        let load_order = ordered_names.iter().map(|name| ids[name]).collect();
        Ok(FrozenMetaRegistry { registries, load_order, ids })
    }
}

/// Builder returned by declaring a registry. Ordering constraints belong to the new
/// registry entry, keeping declaration and its phase relationships together.
pub struct RegistryBuilder<'a> {
    meta: &'a mut MetaRegistry,
    name: String,
}

impl RegistryBuilder<'_> {
    /// Freeze this registry before another registry.
    pub fn before(self, other: &str) -> Self {
        self.meta.order_before(&self.name, other);
        self
    }

    /// Freeze this registry after another registry.
    pub fn after(self, other: &str) -> Self {
        self.meta.order_after(&self.name, other);
        self
    }
}

/// Immutable registry metadata available after the meta-registry phase.
#[derive(Clone, Debug)]
pub struct FrozenMetaRegistry {
    /// Stored in stable identifier order; numeric IDs index this slice.
    registries: Vec<RegistryDescriptor>,
    /// Registry IDs in deterministic dependency order.
    load_order: Vec<RegistryId>,
    ids: BTreeMap<String, RegistryId>,
}

impl FrozenMetaRegistry {
    pub fn registry(&self, id: RegistryId) -> Option<&RegistryDescriptor> {
        self.registries.get(id.0 as usize)
    }

    pub fn find(&self, name: &str) -> Option<RegistryId> {
        self.ids.get(name).copied()
    }

    pub fn registries(&self) -> &[RegistryDescriptor] {
        &self.registries
    }

    pub fn load_order(&self) -> &[RegistryId] {
        &self.load_order
    }
}

fn valid_name(name: &str) -> bool {
    let Some((namespace, path)) = name.split_once(':') else {
        return false;
    };
    !namespace.is_empty() && !path.is_empty() && !name.contains(' ') && !name.contains('\\') && !name.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_mods_can_contribute_without_owning_the_registry() {
        let mut meta = MetaRegistry::new();
        meta.declare("arcana:spell", ModId(1)).unwrap();
        meta.contribute("arcana:spell", ModId(2)).unwrap();
        meta.contribute("arcana:spell", ModId(3)).unwrap();

        let frozen = meta.freeze(&[], &[ModId(1), ModId(2), ModId(3)]).unwrap();
        let spell = frozen
            .registry(frozen.find("arcana:spell").unwrap())
            .unwrap();

        assert_eq!(spell.owner, ModId(1));
        assert_eq!(spell.contributors, [ModId(1), ModId(2), ModId(3)]);
    }

    #[test]
    fn core_runtime_kind_is_preserved_in_frozen_metadata() {
        let mut meta = MetaRegistry::new();
        meta.declare_core("core:block", 0).unwrap();
        meta.declare("base:creative_tab", ModId(1)).unwrap();

        let frozen = meta.freeze(&[], &[ModId(1)]).unwrap();
        let block = frozen.registry(frozen.find("core:block").unwrap()).unwrap();
        let tab = frozen
            .registry(frozen.find("base:creative_tab").unwrap())
            .unwrap();

        assert_eq!(block.owner, ModId(u32::MAX));
        assert_eq!(block.runtime_kind, Some(0));
        assert_eq!(tab.runtime_kind, None);
    }

    #[test]
    fn numeric_ids_do_not_change_with_load_order() {
        let mut meta = MetaRegistry::new();
        meta.declare("zeta:registry", ModId(1)).unwrap();
        meta.declare("alpha:registry", ModId(2)).unwrap();

        let first = meta.freeze(&[], &[]).unwrap();
        assert_eq!(first.find("alpha:registry"), Some(RegistryId(0)));
        assert_eq!(first.find("zeta:registry"), Some(RegistryId(1)));

        let mut meta = MetaRegistry::new();
        meta.declare("zeta:registry", ModId(1)).unwrap();
        meta.declare("alpha:registry", ModId(2)).unwrap();
        let second = meta.freeze(&[], &[]).unwrap();

        assert_eq!(second.find("alpha:registry"), Some(RegistryId(0)));
        assert_eq!(second.find("zeta:registry"), Some(RegistryId(1)));
        assert_eq!(second.load_order(), [RegistryId(0), RegistryId(1)]);
    }

    #[test]
    fn before_and_after_constraints_define_registry_phase_order() {
        let mut meta = MetaRegistry::new();
        meta.registry("core:item", ModId(0))
            .unwrap()
            .after("materials:tool_material");
        meta.registry("materials:tool_material", ModId(1))
            .unwrap()
            .before("base:creative_tab");
        meta.registry("base:creative_tab", ModId(2)).unwrap();
        let frozen = meta.freeze(&[], &[ModId(0), ModId(1), ModId(2)]).unwrap();
        let names: Vec<&str> = frozen
            .load_order()
            .iter()
            .map(|id| frozen.registry(*id).unwrap().name.as_str())
            .collect();
        assert_eq!(names, ["materials:tool_material", "base:creative_tab", "core:item"]);
    }

    #[test]
    fn independent_registry_phases_use_lexical_tie_breaking() {
        let mut meta = MetaRegistry::new();
        meta.declare("zeta:registry", ModId(1)).unwrap();
        meta.declare("alpha:registry", ModId(2)).unwrap();
        let frozen = meta.freeze(&[], &[]).unwrap();
        let names: Vec<&str> = frozen
            .load_order()
            .iter()
            .map(|id| frozen.registry(*id).unwrap().name.as_str())
            .collect();
        assert_eq!(names, ["alpha:registry", "zeta:registry"]);
    }

    #[test]
    fn unknown_dependencies_and_cycles_are_errors() {
        let mut meta = MetaRegistry::new();
        meta.registry("alpha:registry", ModId(1))
            .unwrap()
            .after("missing:registry");
        assert_eq!(
            meta.freeze(&[], &[]).unwrap_err(),
            [RegistryError::UnknownDependency { registry: "alpha:registry".into(), dependency: "missing:registry".into() }]
        );

        let mut meta = MetaRegistry::new();
        meta.registry("alpha:registry", ModId(1))
            .unwrap()
            .after("beta:registry");
        meta.registry("beta:registry", ModId(2))
            .unwrap()
            .after("alpha:registry");
        assert_eq!(
            meta.freeze(&[], &[]).unwrap_err(),
            [RegistryError::DependencyCycle(vec!["alpha:registry".into(), "beta:registry".into()])]
        );
    }

    #[test]
    fn undeclared_contributions_are_errors() {
        let mut meta = MetaRegistry::new();
        assert_eq!(meta.contribute("arcana:spell", ModId(2)), Err(RegistryError::UndeclaredRegistry("arcana:spell".into())));
        meta.declare("arcana:spell", ModId(1)).unwrap();
        assert!(meta.freeze(&[], &[]).is_ok());
    }
}
