use clippy_config::Conf;
use clippy_config::types::{
    SourceItemOrderingCategory, SourceItemOrderingModuleItemGroupings, SourceItemOrderingModuleItemKind,
    SourceItemOrderingTraitAssocItemKind, SourceItemOrderingTraitAssocItemKinds,
    SourceItemOrderingWithinModuleItemGroupings,
};
use clippy_utils::diagnostics::span_lint_and_note;
use clippy_utils::is_cfg_test;
use rustc_attr_data_structures::AttributeKind;
use rustc_hir::{
    Attribute, FieldDef, HirId, IsAuto, ImplItemId, Item, ItemKind, Mod, OwnerId, QPath, TraitItemId, TyKind,
    Variant, VariantData,
};
use rustc_middle::ty::AssocKind;
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_session::impl_lint_pass;
use rustc_span::Ident;

declare_clippy_lint! {
    /// ### What it does
    ///
    /// Confirms that items are sorted in source files as per configuration.
    ///
    /// ### Why restrict this?
    ///
    /// Keeping a consistent ordering throughout the codebase helps with working
    /// as a team, and possibly improves maintainability of the codebase. The
    /// idea is that by defining a consistent and enforceable rule for how
    /// source files are structured, less time will be wasted during reviews on
    /// a topic that is (under most circumstances) not relevant to the logic
    /// implemented in the code. Sometimes this will be referred to as
    /// "bikeshedding".
    ///
    /// The content of items with a representation clause attribute, such as
    /// `#[repr(C)]` will not be checked, as the order of their fields or
    /// variants might be dictated by an external API (application binary
    /// interface).
    ///
    /// ### Default Ordering and Configuration
    ///
    /// As there is no generally applicable rule, and each project may have
    /// different requirements, the lint can be configured with high
    /// granularity. The configuration is split into two stages:
    ///
    /// 1. Which item kinds that should have an internal order enforced.
    /// 2. Individual ordering rules per item kind.
    ///
    /// The item kinds that can be linted are:
    /// - Module (with customized groupings, alphabetical within - configurable)
    /// - Trait (with customized order of associated items, alphabetical within)
    /// - Enum, Impl, Struct (purely alphabetical)
    ///
    /// #### Module Item Order
    ///
    /// Due to the large variation of items within modules, the ordering can be
    /// configured on a very granular level. Item kinds can be grouped together
    /// arbitrarily, items within groups will be ordered alphabetically. The
    /// following table shows the default groupings:
    ///
    /// | Group              | Item Kinds           |
    /// |--------------------|----------------------|
    /// | `modules`          | "mod", "foreign_mod" |
    /// | `use`              | "use"                |
    /// | `macros`           | "macro"              |
    /// | `global_asm`       | "global_asm"         |
    /// | `UPPER_SNAKE_CASE` | "static", "const"    |
    /// | `PascalCase`       | "ty_alias", "opaque_ty", "enum", "struct", "union", "trait", "trait_alias", "impl" |
    /// | `lower_snake_case` | "fn"                 |
    ///
    /// The groups' names are arbitrary and can be changed to suit the
    /// conventions that should be enforced for a specific project.
    ///
    /// All item kinds must be accounted for to create an enforceable linting
    /// rule set. Following are some example configurations that may be useful.
    ///
    /// Example: *module inclusions and use statements to be at the top*
    ///
    /// ```toml
    /// module-item-order-groupings = [
    ///     [ "modules", [ "extern_crate", "mod", "foreign_mod" ], ],
    ///     [ "use", [ "use", ], ],
    ///     [ "everything_else", [ "macro", "global_asm", "static", "const", "ty_alias", "enum", "struct", "union", "trait", "trait_alias", "impl", "fn", ], ],
    /// ]
    /// ```
    ///
    /// Example: *only consts and statics should be alphabetically ordered*
    ///
    /// It is also possible to configure a selection of module item groups that
    /// should be ordered alphabetically. This may be useful if for example
    /// statics and consts should be ordered, but the rest should be left open.
    ///
    /// ```toml
    /// module-items-ordered-within-groupings = ["UPPER_SNAKE_CASE"]
    /// ```
    ///
    /// ### Known Problems
    ///
    /// #### Performance Impact
    ///
    /// Keep in mind, that ordering source code alphabetically can lead to
    /// reduced performance in cases where the most commonly used enum variant
    /// isn't the first entry anymore, and similar optimizations that can reduce
    /// branch misses, cache locality and such. Either don't use this lint if
    /// that's relevant, or disable the lint in modules or items specifically
    /// where it matters. Other solutions can be to use profile guided
    /// optimization (PGO), post-link optimization (e.g. using BOLT for LLVM),
    /// or other advanced optimization methods. A good starting point to dig
    /// into optimization is [cargo-pgo][cargo-pgo].
    ///
    /// #### Lints on a Contains basis
    ///
    /// The lint can be disabled only on a "contains" basis, but not per element
    /// within a "container", e.g. the lint works per-module, per-struct,
    /// per-enum, etc. but not for "don't order this particular enum variant".
    ///
    /// #### Module documentation
    ///
    /// Module level rustdoc comments are not part of the resulting syntax tree
    /// and as such cannot be linted from within `check_mod`. Instead, the
    /// `rustdoc::missing_documentation` lint may be used.
    ///
    /// #### Module Tests
    ///
    /// This lint does not implement detection of module tests (or other feature
    /// dependent elements for that matter). To lint the location of mod tests,
    /// the lint `items_after_test_module` can be used instead.
    ///
    /// ### Example
    ///
    /// ```no_run
    /// trait TraitUnordered {
    ///     const A: bool;
    ///     const C: bool;
    ///     const B: bool;
    ///
    ///     type SomeType;
    ///
    ///     fn a();
    ///     fn c();
    ///     fn b();
    /// }
    /// ```
    ///
    /// Use instead:
    /// ```no_run
    /// trait TraitOrdered {
    ///     const A: bool;
    ///     const B: bool;
    ///     const C: bool;
    ///
    ///     type SomeType;
    ///
    ///     fn a();
    ///     fn b();
    ///     fn c();
    /// }
    /// ```
    ///
    /// [cargo-pgo]: https://github.com/Kobzol/cargo-pgo/blob/main/README.md
    ///
    #[clippy::version = "1.84.0"]
    pub ARBITRARY_SOURCE_ITEM_ORDERING,
    restriction,
    "arbitrary source item ordering"
}

impl_lint_pass!(ArbitrarySourceItemOrdering => [ARBITRARY_SOURCE_ITEM_ORDERING]);

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // Bools are cached feature flags.
pub struct ArbitrarySourceItemOrdering {
    assoc_types_order: SourceItemOrderingTraitAssocItemKinds,
    enable_ordering_for_enum: bool,
    enable_ordering_for_impl: bool,
    enable_ordering_for_module: bool,
    enable_ordering_for_struct: bool,
    enable_ordering_for_trait: bool,
    module_item_order_groupings: SourceItemOrderingModuleItemGroupings,
    module_items_ordered_within_groupings: SourceItemOrderingWithinModuleItemGroupings,
}

impl ArbitrarySourceItemOrdering {
    pub fn new(conf: &'static Conf) -> Self {
        #[allow(clippy::enum_glob_use)] // Very local glob use for legibility.
        use SourceItemOrderingCategory::*;
        Self {
            assoc_types_order: conf.trait_assoc_item_kinds_order.clone(),
            enable_ordering_for_enum: conf.source_item_ordering.contains(&Enum),
            enable_ordering_for_impl: conf.source_item_ordering.contains(&Impl),
            enable_ordering_for_module: conf.source_item_ordering.contains(&Module),
            enable_ordering_for_struct: conf.source_item_ordering.contains(&Struct),
            enable_ordering_for_trait: conf.source_item_ordering.contains(&Trait),
            module_item_order_groupings: conf.module_item_order_groupings.clone(),
            module_items_ordered_within_groupings: conf.module_items_ordered_within_groupings.clone(),
        }
    }

    /// Produces a linting warning for incorrectly ordered impl items.
    fn lint_impl_item(&self, cx: &LateContext<'_>, item: ImplItemId, before_item: ImplItemId) {
        span_lint_and_note(
            cx,
            ARBITRARY_SOURCE_ITEM_ORDERING,
            cx.tcx.def_span(item.owner_id),
            format!(
                "incorrect ordering of impl items (defined order: {:?})",
                self.assoc_types_order
            ),
            Some(cx.tcx.def_span(before_item.owner_id)),
            format!("should be placed before `{}`", cx.tcx.item_name(before_item.owner_id)),
        );
    }

    /// Produces a linting warning for incorrectly ordered item members.
    fn lint_member_name<T: LintContext>(cx: &T, ident: Ident, before_ident: Ident) {
        span_lint_and_note(
            cx,
            ARBITRARY_SOURCE_ITEM_ORDERING,
            ident.span,
            "incorrect ordering of items (must be alphabetically ordered)",
            Some(before_ident.span),
            format!("should be placed before `{}`", before_ident.name),
        );
    }

    fn lint_member_item(cx: &LateContext<'_>, item: &Item<'_>, before_item: &Item<'_>, msg: &'static str) {
        let span = if let Some(ident) = item.kind.ident() {
            ident.span
        } else {
            item.span
        };

        let (before_span, note) = if let Some(ident) = before_item.kind.ident() {
            (ident.span, format!("should be placed before `{}`", ident.name))
        } else {
            (
                before_item.span,
                "should be placed before the following item".to_owned(),
            )
        };

        // This catches false positives where generated code gets linted.
        if span == before_span {
            return;
        }

        span_lint_and_note(cx, ARBITRARY_SOURCE_ITEM_ORDERING, span, msg, Some(before_span), note);
    }

    /// Produces a linting warning for incorrectly ordered trait items.
    fn lint_trait_item(&self, cx: &LateContext<'_>, item: TraitItemId, before_item: TraitItemId) {
        span_lint_and_note(
            cx,
            ARBITRARY_SOURCE_ITEM_ORDERING,
            cx.tcx.def_span(item.owner_id),
            format!(
                "incorrect ordering of trait items (defined order: {:?})",
                self.assoc_types_order
            ),
            Some(cx.tcx.def_span(before_item.owner_id)),
            format!("should be placed before `{}`", cx.tcx.item_name(before_item.owner_id)),
        );
    }
}

impl<'tcx> LateLintPass<'tcx> for ArbitrarySourceItemOrdering {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        if cx
            .tcx
            .hir_attrs(item.hir_id())
            .iter()
            .any(|attr| matches!(attr, Attribute::Parsed(AttributeKind::Repr { .. })))
        {
            // Do not lint items with a `#[repr]` attribute as their layout may be imposed by an external API.
            return;
        }
        match &item.kind {
            ItemKind::Enum(_, _generics, enum_def) if self.enable_ordering_for_enum => {
                let mut cur_v: Option<&Variant<'_>> = None;
                for variant in enum_def.variants {
                    if variant.span.in_external_macro(cx.sess().source_map()) {
                        continue;
                    }

                    if let Some(cur_v) = cur_v
                        && cur_v.ident.name.as_str() > variant.ident.name.as_str()
                        && cur_v.span != variant.span
                    {
                        Self::lint_member_name(cx, variant.ident, cur_v.ident);
                    }
                    cur_v = Some(variant);
                }
            },
            ItemKind::Struct(_, _generics, VariantData::Struct { fields, .. }) if self.enable_ordering_for_struct => {
                let mut cur_f: Option<&FieldDef<'_>> = None;
                for field in *fields {
                    if field.span.in_external_macro(cx.sess().source_map()) {
                        continue;
                    }

                    if let Some(cur_f) = cur_f
                        && cur_f.ident.name.as_str() > field.ident.name.as_str()
                        && cur_f.span != field.span
                    {
                        Self::lint_member_name(cx, field.ident, cur_f.ident);
                    }
                    cur_f = Some(field);
                }
            },
            ItemKind::Trait(is_auto, _safety, _ident, _generics, _generic_bounds, item_ref)
                if self.enable_ordering_for_trait && *is_auto == IsAuto::No =>
            {
                let mut cur_t: Option<(TraitItemId, Ident)> = None;

                for &item in *item_ref {
                    let span = cx.tcx.def_span(item.owner_id);
                    let ident = cx.tcx.item_ident(item.owner_id);
                    if span.in_external_macro(cx.sess().source_map()) {
                        continue;
                    }

                    if let Some((cur_t, cur_ident)) = cur_t {
                        let cur_t_kind = convert_assoc_item_kind(cx, cur_t.owner_id);
                        let cur_t_kind_index = self.assoc_types_order.index_of(&cur_t_kind);
                        let item_kind = convert_assoc_item_kind(cx, item.owner_id);
                        let item_kind_index = self.assoc_types_order.index_of(&item_kind);

                        if cur_t_kind == item_kind && cur_ident.name.as_str() > ident.name.as_str() {
                            Self::lint_member_name(cx, ident, cur_ident);
                        } else if cur_t_kind_index > item_kind_index {
                            self.lint_trait_item(cx, item, cur_t);
                        }
                    }
                    cur_t = Some((item, ident));
                }
            },
            ItemKind::Impl(trait_impl) if self.enable_ordering_for_impl => {
                let mut cur_t: Option<(ImplItemId, Ident)> = None;

                for &item in trait_impl.items {
                    let span = cx.tcx.def_span(item.owner_id);
                    let ident = cx.tcx.item_ident(item.owner_id);
                    if span.in_external_macro(cx.sess().source_map()) {
                        continue;
                    }

                    if let Some((cur_t, cur_ident)) = cur_t {
                        let cur_t_kind = convert_assoc_item_kind(cx, cur_t.owner_id);
                        let cur_t_kind_index = self.assoc_types_order.index_of(&cur_t_kind);
                        let item_kind = convert_assoc_item_kind(cx, item.owner_id);
                        let item_kind_index = self.assoc_types_order.index_of(&item_kind);

                        if cur_t_kind == item_kind && cur_ident.name.as_str() > ident.name.as_str() {
                            Self::lint_member_name(cx, ident, cur_ident);
                        } else if cur_t_kind_index > item_kind_index {
                            self.lint_impl_item(cx, item, cur_t);
                        }
                    }
                    cur_t = Some((item, ident));
                }
            },
            _ => {}, // Catch-all for `ItemKinds` that don't have fields.
        }
    }

    fn check_mod(&mut self, cx: &LateContext<'tcx>, module: &'tcx Mod<'tcx>, _: HirId) {
        struct CurItem<'a> {
            item: &'a Item<'a>,
            order: usize,
            name: Option<String>,
        }
        let mut cur_t: Option<CurItem<'_>> = None;

        if !self.enable_ordering_for_module {
            return;
        }

        let items = module.item_ids.iter().map(|&id| cx.tcx.hir_item(id));

        // Iterates over the items within a module.
        //
        // As of 2023-05-09, the Rust compiler will hold the entries in the same
        // order as they appear in the source code, which is convenient for us,
        // as no sorting by source map/line of code has to be applied.
        //
        for item in items {
            if is_cfg_test(cx.tcx, item.hir_id()) {
                continue;
            }

            if item.span.in_external_macro(cx.sess().source_map()) {
                continue;
            }

            if let Some(ident) = item.kind.ident() {
                if ident.name.as_str().starts_with('_') {
                    // Filters out unnamed macro-like impls for various derives,
                    // e.g. serde::Serialize or num_derive::FromPrimitive.
                    continue;
                }

                if ident.name == rustc_span::sym::std && item.span.is_dummy() {
                    if let ItemKind::ExternCrate(None, _) = item.kind {
                        // Filters the auto-included Rust standard library.
                        continue;
                    }
                    if cfg!(debug_assertions) {
                        rustc_middle::bug!("unknown item: {item:?}");
                    }
                }
            } else if let ItemKind::Impl(_) = item.kind
                && get_item_name(item).is_some()
            {
                // keep going below
            } else {
                continue;
            }

            let item_kind = convert_module_item_kind(&item.kind);
            let grouping_name = self.module_item_order_groupings.grouping_name_of(&item_kind);
            let module_level_order = self
                .module_item_order_groupings
                .module_level_order_of(&item_kind)
                .unwrap_or_default();

            if let Some(cur_t) = cur_t.as_ref() {
                use std::cmp::Ordering; // Better legibility.
                match module_level_order.cmp(&cur_t.order) {
                    Ordering::Less => {
                        Self::lint_member_item(
                            cx,
                            item,
                            cur_t.item,
                            "incorrect ordering of items (module item groupings specify another order)",
                        );
                    },
                    Ordering::Equal if item_kind == SourceItemOrderingModuleItemKind::Use => {
                        // Skip ordering use statements, as these should be ordered by rustfmt.
                    },
                    Ordering::Equal
                        if (grouping_name.is_some_and(|grouping_name| {
                            self.module_items_ordered_within_groupings.ordered_within(grouping_name)
                        }) && cur_t.name > get_item_name(item)) =>
                    {
                        Self::lint_member_item(
                            cx,
                            item,
                            cur_t.item,
                            "incorrect ordering of items (must be alphabetically ordered)",
                        );
                    },
                    Ordering::Equal | Ordering::Greater => {
                        // Nothing to do in this case, they're already in the right order.
                    },
                }
            }

            // Makes a note of the current item for comparison with the next.
            cur_t = Some(CurItem {
                item,
                order: module_level_order,
                name: get_item_name(item),
            });
        }
    }
}

/// Converts a [`ty::AssocKind`] to a [`SourceItemOrderingTraitAssocItemKind`].
///
/// This is implemented here because `rustc_hir` is not a dependency of
/// `clippy_config`.
fn convert_assoc_item_kind(cx: &LateContext<'_>, owner_id: OwnerId) -> SourceItemOrderingTraitAssocItemKind {
    let kind = cx.tcx.associated_item(owner_id.def_id).kind;

    #[allow(clippy::enum_glob_use)] // Very local glob use for legibility.
    use SourceItemOrderingTraitAssocItemKind::*;
    match kind {
        AssocKind::Const{..} => Const,
        AssocKind::Type {..}=> Type,
        AssocKind::Fn { .. } => Fn,
    }
}

/// Converts a [`rustc_hir::ItemKind`] to a
/// [`SourceItemOrderingModuleItemKind`].
///
/// This is implemented here because `rustc_hir` is not a dependency of
/// `clippy_config`.
fn convert_module_item_kind(value: &ItemKind<'_>) -> SourceItemOrderingModuleItemKind {
    #[allow(clippy::enum_glob_use)] // Very local glob use for legibility.
    use SourceItemOrderingModuleItemKind::*;
    match value {
        ItemKind::ExternCrate(..) => ExternCrate,
        ItemKind::Use(..) => Use,
        ItemKind::Static(..) => Static,
        ItemKind::Const(..) => Const,
        ItemKind::Fn { .. } => Fn,
        ItemKind::Macro(..) => Macro,
        ItemKind::Mod(..) => Mod,
        ItemKind::ForeignMod { .. } => ForeignMod,
        ItemKind::GlobalAsm { .. } => GlobalAsm,
        ItemKind::TyAlias(..) => TyAlias,
        ItemKind::Enum(..) => Enum,
        ItemKind::Struct(..) => Struct,
        ItemKind::Union(..) => Union,
        ItemKind::Trait(..) => Trait,
        ItemKind::TraitAlias(..) => TraitAlias,
        ItemKind::Impl(..) => Impl,
    }
}

/// Gets the item name for sorting purposes, which in the general case is
/// `item.ident.name`.
///
/// For trait impls, the name used for sorting will be the written path of
/// `item.self_ty` plus the written path of `item.of_trait`, joined with
/// exclamation marks. Exclamation marks are used because they are the first
/// printable ASCII character.
///
/// Trait impls generated using a derive-macro will have their path rewritten,
/// such that for example `Default` is `$crate::default::Default`, and
/// `std::clone::Clone` is `$crate::clone::Clone`. This behaviour is described
/// further in the [Rust Reference, Paths Chapter][rust_ref].
///
/// [rust_ref]: https://doc.rust-lang.org/reference/paths.html#crate-1
fn get_item_name(item: &Item<'_>) -> Option<String> {
    match item.kind {
        ItemKind::Impl(im) => {
            if let TyKind::Path(path) = im.self_ty.kind {
                match path {
                    QPath::Resolved(_, path) => {
                        let segs = path.segments.iter();
                        let mut segs: Vec<String> = segs.map(|s| s.ident.name.as_str().to_owned()).collect();

                        if let Some(of_trait) = im.of_trait {
                            let mut trait_segs: Vec<String> = of_trait
                                .path
                                .segments
                                .iter()
                                .map(|s| s.ident.name.as_str().to_owned())
                                .collect();
                            segs.append(&mut trait_segs);
                        }

                        segs.push(String::new());
                        Some(segs.join("!!"))
                    },
                    QPath::TypeRelative(_, _path_seg) => {
                        // This case doesn't exist in the clippy tests codebase.
                        None
                    },
                    QPath::LangItem(_, _) => None,
                }
            } else {
                // Impls for anything that isn't a named type can be skipped.
                None
            }
        },
        _ => item.kind.ident().map(|name| name.as_str().to_owned()),
    }
}
