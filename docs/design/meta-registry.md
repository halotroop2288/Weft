# Meta-registry and phased registry loading

**Status:** Draft design for the registry refactor  
**Scope:** ABI, engine loader, SDK registration API, and ownership of the built-in registries.

## Summary

The engine ABI should know about exactly one registry: the **meta-registry**. The meta-registry describes the registries available to the game. It does not contain the entries belonging to those registries.

A registry has a namespaced identifier and one declaring mod. Other mods may contribute entries to it, and may edit or remove entries according to the registry's supported operations. Declaring a registry does not grant exclusive ownership of its contents.

Startup has a hard phase boundary:

1. Load mods and collect registry declarations.
2. Resolve registry load order.
3. Load every registry and all contributions to it.
4. Only after every registry has loaded, run the ordinary mod-content phase.
5. Freeze and validate the resulting game content.

This is intended to replace the current hardcoded `entry_kind` list and the single-phase registration path.

## Ownership

There are three relevant ownership levels:

- **Engine ABI:** owns the meta-registry protocol and generic registry access only. It does not define `block`, `item`, `creative_tab`, or any other game registry.
- **Core:** declares registries required for the game to function. These are game requirements, not ABI constants. Core can add further registries as the game evolves.
- **Base:** declares standard-game registries that are useful but not fundamental to the engine. Creative tabs are an example of a registry that can live here.
- **Other mods:** may declare new registries and contribute entries to registries declared by any loaded mod.

Core and base are ordinary registry providers from the meta-registry's perspective. They should not require special-case ABI paths.

A registry identifier is namespaced (for example, `weft:block`, `base:creative_tab`, or `arcana:spell`). Runtime numeric IDs are assigned by the engine after discovery. Those numeric IDs are transient implementation details; namespaced identifiers are the persistent and public identity.

## Declarations and contributions are separate

A registry declaration identifies the mod responsible for providing that registry:

```rust,ignore
RegistryDeclaration {
    id: "arcana:spell",
    owner: "arcana",
}
```

A contribution identifies a mod providing entries to an already declared registry:

```rust,ignore
RegistryContribution {
    registry: "arcana:spell",
    contributor: "spell_pack",
}
```

The contributor does not declare or own the registry. It only needs the registry to exist and to expose the generic entry operations it uses.

The loader should enforce these rules:

- A namespaced registry ID has exactly one declaring owner.
- Any loaded mod may contribute to a declared registry.
- A contribution to an undeclared registry is a load error.
- A mod may contribute to a registry declared by another mod without depending on or being owned by that mod. A normal mod dependency can still be used when the contributor needs the owner's code or assets.
- Contributions are ordered deterministically by the existing mod load order. Registry order and mod contribution order are separate concepts.
- Registry IDs are assigned deterministically by identifier, not by the order in which mods happen to initialize.

Registry ownership describes who defines the registry and its loading contract. It does not prevent other mods from adding, editing, replacing, or removing entries.

## Startup phases

### 1. Discover mods

Load and validate mod libraries as today. Establish the final set of enabled mods and their stable `ModId` values before any registration callback runs.

### 2. Collect registry declarations

Add a registry-declaration phase to the mod ABI/SDK. It must run before any registry contents are loaded.

At the end of this phase, the engine has the complete set of registry descriptors and knows which mod declared each one. Declarations should be data-driven; the engine must not need a new enum variant or ABI constant when a mod adds a registry.

### 3. Resolve registry load order

Resolve registry ordering from the declared registries and the user's configured order.

The registry order is intentionally user/mod-author controlled. It is not inferred from mod load order. Registry dependencies can be expressed as ordering constraints to provide a sensible default, but the configured order is authoritative: the engine must not silently move a registry to a different position just to make a dependency work.

An order that places a dependent registry before the registry it needs is allowed to fail during loading. The engine should report useful errors where it can, but it should not secretly repair the selected order.

The configuration should use stable registry identifiers, not numeric IDs. A line-oriented `config/registry_order.txt` is consistent with the existing `step_order.txt` approach. A partial list can override the relative order of listed registries; registries not listed are appended in a deterministic default order. Duplicate or unknown IDs should be reported rather than silently interpreted as a different registry.

### 4. Load registry contents

Process registries in the resolved registry order.

For each registry, run the registry-loading callback for each mod that contributes to it, in deterministic mod load order. The callback receives the target registry identity/handle, so a contributor can provide entries without owning the registry.

A registry can rely on registries that have already loaded. It cannot rely on later registries unless the mod author deliberately chooses an order that makes this fail.

The registry owner defines the registry's loading contract. In the initial implementation, the generic entry/component machinery should remain the common storage and editing model. Registry-specific interpretation can remain in specialized core/base code.

### 5. Load ordinary mod content

Only after every registry has completed its registry-loading callbacks should the existing ordinary content initialization callback run.

This phase is for the rest of mod content: recipes, world-generation content, configuration, hooks, runtime systems, and other content that expects all registries to be available.

The engine must not interleave this phase with registry loading. That phase boundary is the central guarantee of this design.

### 6. Freeze and validate

Freeze registry identifiers and contents, build runtime tables, resolve references, and validate the resulting content. A registry's runtime IDs and component tables are exposed only after this point.

## ABI and SDK shape

The ABI should expose generic registry metadata and generic registry access:

- An opaque `RegistryId` for a registry in the current loaded game.
- A registry descriptor with its namespaced ID and declaring owner.
- A dynamically sized registry/component table.
- Generic lookup by registry identifier and generic entry access by `RegistryId`.

The ABI's `ComponentTable` is a `Slice<RegistryTable>` indexed directly by `RegistryId`; it has no built-in compatibility prefix. The engine keeps private numeric slots for specialized runtime pipelines and maps those slots to registry IDs when it exports component tables.

`EntryDesc.registry` and `Host::find` both use `RegistryId`. The ABI no longer exports an `entry_kind` module or any fixed list of built-in game registries.

The SDK can retain ergonomic typed handles for core/base registries. Their marker types should resolve their namespaced registry identifier through the meta-registry rather than contain an ABI-defined numeric constant. A generic registry/entry API should be available for mod-defined registries.

### Pre-initializer declarations

Registry declarations are exported separately from the existing `weft_mod` entry point. The SDK macro is:

```rust,ignore
weft_sdk::declare_registries!(
    registries: ["arcana:spell"],
    contributes_to: ["base:creative_tab"],
);
```

Both lists are optional. `registries` declares registries owned by this mod; `contributes_to` declares that this mod will contribute entries to registries owned by other mods. A mod with a non-empty `contributes_to` list must also export `weft_registry_init`. The loader reads this metadata before calling any ordinary `init` callback. The existing `declare_mod!(deps, init)` syntax and `weft_mod` symbol remain unchanged.

At the ABI level this uses an optional `weft_registries` symbol returning `RegistryDeclarations`. Mods that do not export the symbol have no registry declarations. The loader first gathers every mod's owned registries, then resolves contributions, so a contributor can be loaded before the registry owner without making declaration discovery order-dependent.

After this phase, the SDK exposes `registry_id("namespace:path") -> Option<RegistryId>` and `registry_name(id) -> Option<&'static str>`. The numeric ID is a session-local lookup key; namespaced strings remain the persistent identity.

Mods can separately export a per-registry callback:

```rust,ignore
pub static POWER: Component<u32> = Component::named("power");

fn load_registry(reg: &mut Registry, registry: RegistryId) {
    reg.entry("ember").set(&POWER, 3);
}
weft_sdk::declare_registry_init!(load_registry);
```

The loader calls it once for each registry the mod owns or contributes to, in registry load order and contributor mod order. Only after every such callback finishes does the existing `weft_mod`/ordinary initializer run. The generic entry builder supports new entries, edits, replacements, removals, tags, and typed component values. Frozen entries can be enumerated with `registry_entry_count(registry)`, looked up with `registry_entry_id(registry, "namespace:path")`, and named with `registry_entry_name(registry, id)`. A component reads its typed value with `POWER.on_registry(registry, id)`; a tag uses `TAG.on_registry(registry, id)`. The SDK also exposes `components_of_registry(registry, id)` and `registry_entries_with(registry, component)` through the dynamic component table. Component names must use one consistent schema across all entries in a registry.

The loader reads the partial override list from `config/registry_order.txt`. Contributor ordering follows the existing mod dependency/after/before order. Generic entries are folded independently per registry and stored without assigning them any built-in game semantics.

The mod ABI is source-hash versioned. Mods built against the updated ABI must be rebuilt; the optional declaration symbol lets mods that do not declare registries continue using the unchanged `declare_mod!` source API.

## Specialized runtime behavior

Making registry identities dynamic does not require making every registry's runtime semantics generic.

For example, the engine may still have optimized block collision, item-stack, entity, or biome systems. Those systems should be core-owned consumers of the corresponding registry data, rather than evidence that the ABI needs a hardcoded registry-kind list.

The generic registry layer handles identity, ownership, ordering, entry contributions, components, and generic edits. Core/base code handles the meaning and specialized runtime representation of its registries.

This also makes it possible to add a mod-owned data registry that the engine never interprets. It can still be queried and edited through generic entry/component APIs.

## Migration plan

The change should be staged to keep each step reviewable:

1. **Introduce meta-registry types and dynamic tables.** Registry IDs/descriptors and component tables indexed directly by `RegistryId` are in place.
2. **Split mod initialization into phases.** Add registry declaration and per-registry loading callbacks while retaining the ordinary content callback as the final phase.
3. **Add generic SDK contribution APIs.** Permit any mod to contribute to a registry declared by another mod.
4. **Move built-in registry declarations out of the ABI.** Core declares required registries; base declares optional standard-game registries.
5. **Migrate existing built-ins incrementally.** Keep specialized runtime builders where needed. Move creative tabs to base as a concrete early example.
6. **Remove compatibility constants and the old `entry_kind` list.** The ABI no longer contains the list; specialized dispatch constants now live inside the engine only.

Do not attempt to migrate all specialized content structures in one commit. The first functional milestone should prove that a mod can declare a generic registry, a second mod can contribute entries to it, and ordinary content initialization does not run until all registries are loaded.

## Acceptance tests

The implementation is ready for the next stage when tests demonstrate:

1. A core-owned registry is available to base and third-party mods.
2. A mod declares `example:spell`; another mod contributes entries to it without declaring it.
3. A third mod's ordinary content callback runs only after every registry contribution callback has completed.
4. Registry order is independent of mod load order and is deterministic across runs.
5. An explicit registry order is honored even when it creates an invalid dependency; the loader does not silently reorder it.
6. Unknown registry contributions and duplicate registry declarations produce clear load errors.
7. A mod-defined generic registry supports entry lookup, components/tags, edits, and removal without engine-specific knowledge of what its entries mean.

## Current implementation points

The fork currently has the following relevant constraints:

- `crates/abi/src/lib.rs`: `EntryDesc.registry` and `Host::find` use `RegistryId`; `ComponentTable.registries` is a `Slice<RegistryTable>` indexed directly by that ID. The ABI has no `entry_kind` module.
- `crates/abi/src/lib.rs`: `ModDecl` still exposes one `init` callback; a separate optional `weft_registries` symbol exports pre-initializer metadata.
- `crates/sdk/src/lib.rs`: `declare_registries!` exports owned registry and contribution declarations without changing `declare_mod!`.
- `crates/sdk/src/content.rs`: `registry_id`, `registry_name`, `registry_entry_count`, `registry_entry_id`, and `registry_entry_name` expose frozen meta-registry and generic entry lookup.
- `crates/sdk/src/component.rs`: component and tag reads, enumeration, and entry counts address component tables directly by `RegistryId`; there is no numeric kind-slot conversion in the SDK.
- `crates/sdk/src/lib.rs`: `declare_registry_init!` exports the separate per-registry callback without changing `declare_mod!`.
- `crates/engine/src/modloader.rs`: engine-required registries are declared as `core:*` entries owned by the reserved engine owner; per-registry callbacks run sequentially in registry/contributor order before ordinary mod initialization.
- `crates/engine/src/content.rs` and `crates/engine/src/entries.rs`: generic entries are folded per registry and retained in `Content::registry_entries`.
- `crates/engine/src/content.rs`: `freeze` merges all mod registrars and builds the game's specialized content tables.
- `crates/sdk/src/content.rs`: typed SDK kinds and entry builders identify registries by namespaced registry names and resolve them to `RegistryId` at runtime.

These are the primary refactor points. The draft now has pre-initializer declarations, per-registry callbacks, generic entry contributions, frozen generic entry lookup, and component tables indexed directly by `RegistryId`. The ABI no longer contains built-in registry constants. Remaining work is to reduce the engine's private specialized dispatch map and continue moving registry-specific behavior into core/base consumers.


## Base-owned creative tabs

The base mod now declares `base:creative_tab` during the pre-initializer phase and creates its vanilla tabs from `declare_registry_init!`. The engine's creative-tab freezer consumes the frozen generic registry entries rather than the engine-private `kind::CREATIVE_TAB` dispatch slot. The SDK's `CreativeTab` handle and `Registry::creative_tab`, `edit`, `replace`, and `remove` route through the base-owned registry.

The engine retains a private creative-tab dispatch slot for its existing catalog code; it is not an ABI registry identifier and is not used to index the exported component table.


The SDK's typed content handles, entry builders, component reads, and tag reads now identify registries by namespaced registry name or `RegistryId`. `Host::find` accepts `RegistryId`, and `ComponentTable.registries` is indexed directly by it. The engine still uses private numeric dispatch slots internally for specialized runtime builders, but those slots are no longer part of the ABI or SDK.


---

## Revised design direction: dependency-ordered registry phases

**This section is the current architectural goal and supersedes the earlier design above wherever they conflict.** In particular, registry order is no longer intended to come from `config/registry_order.txt`, and registry contents are not intended to wait until one final global freeze.

### Core purpose

The meta-registry exists so that the engine, base game, and mods can declare registries, and so mods can decide the order in which those registries are populated and frozen.

- **Core** declares only registries the engine/game absolutely requires to function.
- **Base** declares optional standard voxel-game registries, such as `base:creative_tab`.
- **Mods** may declare arbitrary registries, such as `materials:tool_material`, and other mods may contribute to them.
- Registry ownership and contribution remain separate: declaring a registry establishes its owner; contributing allows a mod to populate it.
- The engine may retain specialized runtime implementations for particular registries. Those implementation details must not restrict which registries mods can declare.

### Registry builders define the phase order

Each meta-registry entry has `before` and `after` builder functions. These constraints define the registry dependency graph and therefore the explicit order in which registries are initialized and frozen.

They are constraints on **registry entries**, not mod load-order constraints. The existing mod-level `deps`, `after`, and `before` continue to describe mod loading and conflict precedence; they are a separate mechanism.

The engine must resolve the complete graph after all mods have declared their registries. It then performs a deterministic topological ordering and executes one registry phase at a time. A registry's `after` dependencies must already be frozen when its phase begins; its `before` constraints ensure it runs ahead of the named registries. Missing references, duplicate declarations, and cycles must produce actionable load errors. Do not silently reorder registrations to conceal an invalid dependency graph.

The resolved graph—not a separate configuration file or incidental mod discovery order—is the source of truth for registry phase order. Independent entries need a deterministic tie-break so runs are reproducible.

### One mod declaration API with preinit and init

The intended SDK shape is:

```rust,ignore
declare_mod!(
    deps: [],
    after: [],
    before: [],
    preinit: preinit,
    init: init,
);

fn preinit(r: &mut MetaRegistry) {
    r.registry("base:tool_material")
        .before("core:item");
}

fn init(r: &mut Registry) {
    // Populate whichever registry phase the engine is currently running.
}
```

- `preinit` runs after all eligible mod libraries have been loaded and before any registry contents are initialized. Calling `registry("namespace:path")` creates a new registry entry owned by the calling mod and returns that entry's builder; `before` and `after` configure ordering on that newly declared entry. The namespace is simply the part before the colon in the registry identifier.
- The engine declares its mandatory core registries before mod preinitialization. Base declares its optional registries through the same public builder API.
- `init` is the registration callback used during registry phases. The engine invokes it with a `Registry` builder scoped to the registry currently being initialized, for each registry the mod owns or contributes to.
- Registry population remains in `init`; `preinit` describes registry existence and ordering, not entry contents.
- This replaces the separate `declare_registries!` and `declare_registry_init!` symbols with one coherent `declare_mod!` lifecycle. Mods without a `preinit` may omit it if the API supports that compatibly.

The exact builder method names and ABI representation can be settled during implementation, but the lifecycle and separation of responsibilities above are requirements.

### Freeze incrementally, not after all callbacks

For each registry in topological order:

1. Make the registry's phase active.
2. Invoke the `init` callback of each contributor in deterministic mod load order, with a builder scoped to that registry.
3. Resolve and validate that registry's pending entries and references.
4. Freeze its entries and publish its lookup/component data.
5. Proceed to the next registry.

A later phase can query registries that have already frozen. It must not observe partially populated or not-yet-frozen registries as though they were complete.

This is necessary for dependency-driven generation. For example, a mod may declare `materials:tool_material` and register material definitions there. An item generator may need those definitions to generate tools in `core:item`. The item phase must be ordered after the material phase, so the generator sees the complete frozen material registry.

### Functionally transparent deferral

Registration should be insensitive to the order in which mods happen to be loaded or their registration code is encountered. The engine may buffer or defer registration operations until the appropriate registry phase, after all mods have been loaded and the full dependency graph is known.

Deferral must be semantically transparent: it must preserve registration intent, contributor identity, mod load-order precedence, and error reporting. It must not run a generator before its declared dependencies are frozen.

In particular, defer **work that computes registrations**, not just the final entry writes. If a generator needs tool materials to construct item entries, the generator itself must execute in the item phase after the material registry has frozen. Buffering an already-computed item entry would be too late to solve that dependency.

The system should make ordinary registration code straightforward: mods express what registry a registration targets and what registry data it depends on; the engine schedules the operation. Avoid requiring authors to manually manage queues or duplicate registration code across phases.

### Example: generated tools

A material-provider mod could declare and populate:

```rust,ignore
fn preinit(r: &mut MetaRegistry) {
    r.registry("materials:tool_material")
        .before("core:item");
}

fn init(r: &mut Registry) {
    // During materials:tool_material, register iron, copper, etc.
}
```

The tool-generation mod contributes to the existing core item registry. It does not redeclare it; the material registry's owner supplies the ordering constraint above:

```rust,ignore
fn preinit(r: &mut MetaRegistry) {
    r.contribute_to("core:item");
}

fn init(r: &mut Registry) {
    // During core:item, enumerate frozen tool materials and generate items.
}
```

The tool generator may be discovered or registered earlier in source/discovery order. That must not cause it to run early: the dependency graph schedules it in the item phase after all material contributions have completed and the material registry is frozen.

### Revised acceptance criteria

The architecture is not complete until tests demonstrate:

1. Core declares only mandatory registries; base and mods can declare additional registries through preinit.
2. A mod can declare an arbitrary registry, and other mods can contribute entries without owning it.
3. Registry-level `before` and `after` constraints determine a deterministic topological phase order independently of mod discovery order.
4. Missing registry references and cycles fail with useful diagnostics.
5. Each registry is fully populated and frozen before any dependent registry's `init` callback runs.
6. A later phase can query earlier frozen registry entries and generate its own entries from them.
7. Registration/generation can be deferred transparently until all mods are loaded and the registry graph is known, without changing mod precedence or losing errors.
8. A deliberately early generator cannot observe an incomplete dependency registry; its work is scheduled in the correct phase.
9. The final runtime tables are assembled from the incrementally frozen registries without requiring a second global pass that changes their established order.

### Current implementation status

The branch now resolves registry order from the meta-registry dependency graph and freezes generic registries incrementally. For each generic registry, the loader runs its contributors in mod load order, folds and validates the entries, then publishes immutable entry lookup data before starting the next registry phase. Later registry callbacks can query earlier generic registries through the SDK's entry lookup and typed component/tag APIs. Reads of a registry that has not frozen yet report a registration error rather than appearing as an empty registry.

The lifecycle is still transitional in several ways:

- `declare_mod!` now accepts an optional `preinit` callback. Its `MetaRegistry` builder can declare registries, add `before`/`after` constraints, and declare contributions. The loader runs every preinit before resolving contributions, so declarations may refer to registries owned by mods loaded later.
- The older optional `weft_registries` symbol remains supported for compatibility, and population still uses the separate `weft_registry_init` callback. The final unified lifecycle, where `init` handles both registry phases and ordinary initialization, is not implemented yet.
- Incremental publication currently covers generic registries. Specialized core registries still collect their `RawGame` entries for the later game-content freeze, so they are not yet queryable as frozen registry tables during subsequent phases.
- Ordinary `declare_mod!` initialization still runs after all registry callbacks.
- The engine retains specialized runtime builders; their phase-specific freeze and publication must be integrated without re-folding generic registries.

Treat the separate callback symbols and global specialized-content pass as transitional rather than design constraints.
