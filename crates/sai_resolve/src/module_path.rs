//! Resolve the module-like prefix of an already-rooted HIR path.
//!
//! The caller resolves the first name (including local/generic/module scope
//! precedence) and passes its `Res` here. This walker only follows module and
//! interface members. It does not resolve generic arguments, type-relative
//! tails, locals, or owner-level diagnostics.

use crate::module_member_lookup::{ModuleMemberIndex, ModuleMemberLookup, ModuleTarget};
use crate::package_scopes::Res;
use sai_hir::types::PathSegment;
use sai_hir::{DefId, PackageId, Symbol};

/// Why module-prefix traversal stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModulePathStop {
    /// Every supplied segment was consumed.
    SegmentsExhausted,
    /// The current resolution is not a module or interface; the current and
    /// following segments remain for the caller to interpret.
    NonModule,
    /// An earlier error resolution or an error-symbol segment already
    /// represents a diagnosed syntax/name error.
    SuppressedError,
}

/// The resolved prefix and the first segment the caller still needs to handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModulePathPrefix {
    /// Resolution after the consumed prefix, or `Res::Err` for a suppressed
    /// error segment.
    pub base: Res,
    /// Number of entries consumed from the supplied segment slice. An
    /// error-symbol segment counts as consumed so callers can suppress the
    /// rest of the chain without trying to resolve it.
    pub consumed_segments: usize,
    pub stop: ModulePathStop,
}

/// A failed lookup while the current resolution was known to be module-like.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModulePathError {
    /// The module-like target has no member with this spelling.
    MissingMember {
        segment_index: usize,
        name: Symbol,
        span: sai_hir::Span,
        target: ModuleTarget,
    },
    /// A member exists, but its effective visibility does not permit access.
    InaccessibleMember {
        segment_index: usize,
        name: Symbol,
        span: sai_hir::Span,
        declaration_span: sai_vfs::Span,
    },
    /// The input was a module/interface `Res`, but its source package scope or
    /// WIT view scope was not supplied to `ModuleMemberIndex`.
    MissingTargetScope { segment_index: usize, target: DefId },
}

/// Consume the module/interface portion of a path after its first name has
/// already been resolved.
///
/// `segments` contains only names after the starting `Res`. Generic arguments
/// attached to a segment do not affect member lookup and remain unchanged in
/// the borrowed input. Indices in errors and `consumed_segments` are relative
/// to this slice.
///
/// A `Res::Err` starting point, an error resolution reached through a member,
/// or a `Symbol::ERROR` segment returns `SuppressedError` instead of
/// generating a cascading lookup error. If a non-module resolution is reached,
/// the walker succeeds and leaves that segment and the remaining suffix for
/// type checking/value handling. A module-like target missing from the index
/// is an error rather than a valid non-module stop, so incomplete package
/// scope input cannot silently truncate a qualified path.
pub fn resolve_module_prefix(
    starting_res: Res,
    segments: &[PathSegment<'_>],
    requesting_package: PackageId,
    members: &ModuleMemberIndex<'_>,
) -> Result<ModulePathPrefix, ModulePathError> {
    let mut current = starting_res;
    let mut consumed_segments = 0;

    for (segment_index, segment) in segments.iter().enumerate() {
        if current == Res::Err {
            return Ok(ModulePathPrefix {
                base: Res::Err,
                consumed_segments,
                stop: ModulePathStop::SuppressedError,
            });
        }

        if segment.name.is_error() {
            return Ok(ModulePathPrefix {
                base: Res::Err,
                consumed_segments: segment_index + 1,
                stop: ModulePathStop::SuppressedError,
            });
        }

        let Some(target) = ModuleTarget::from_res(current) else {
            return Ok(ModulePathPrefix {
                base: current,
                consumed_segments,
                stop: ModulePathStop::NonModule,
            });
        };

        match members.lookup_target(target, segment.name.sym, requesting_package) {
            ModuleMemberLookup::Found(binding) => {
                current = binding.res;
                consumed_segments += 1;
                if current == Res::Err {
                    return Ok(ModulePathPrefix {
                        base: Res::Err,
                        consumed_segments,
                        stop: ModulePathStop::SuppressedError,
                    });
                }
            }
            ModuleMemberLookup::WitMember(res) => {
                current = res;
                consumed_segments += 1;
                if current == Res::Err {
                    return Ok(ModulePathPrefix {
                        base: Res::Err,
                        consumed_segments,
                        stop: ModulePathStop::SuppressedError,
                    });
                }
            }
            ModuleMemberLookup::Missing => {
                return Err(ModulePathError::MissingMember {
                    segment_index,
                    name: segment.name.sym,
                    span: segment.name.span,
                    target,
                });
            }
            ModuleMemberLookup::Inaccessible(binding) => {
                return Err(ModulePathError::InaccessibleMember {
                    segment_index,
                    name: segment.name.sym,
                    span: segment.name.span,
                    declaration_span: binding.name_span,
                });
            }
            // `target` came from a module/interface `Res`, so this result
            // means the index was built without the scope that owns it.
            ModuleMemberLookup::NotModule => {
                return Err(ModulePathError::MissingTargetScope {
                    segment_index,
                    target: target.def_id(),
                });
            }
        }
    }

    let stop = if current == Res::Err {
        ModulePathStop::SuppressedError
    } else {
        ModulePathStop::SegmentsExhausted
    };
    Ok(ModulePathPrefix {
        base: current,
        consumed_segments,
        stop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_member_lookup::ModuleMemberIndex;
    use crate::package_scopes::{
        Binding, BindingOrigin, DefKind, InterfaceScope, Phase1ModuleScopes, Phase1PackageScopes,
    };
    use sai_hir::types::{GenericArgs, Name, PathSegment};
    use sai_hir::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, LocalDefId,
        OwnerId, PackageId, Span, SpanCtxt,
    };
    use sai_intern::{Interner, ItemLocalId, PackageInterner};
    use sai_package::version::Version;
    use sai_vfs::{FileId, Span as SourceSpan, TextRange, TextSize};
    use sai_wit::{WitUnit, WitView, WitViewDefKind, load_embedded_wasi_package};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn package_id(packages: &PackageInterner, name: &str) -> PackageId {
        packages.intern(name).expect("package interns")
    }

    fn local_def_id(
        package: PackageId,
        packages: &PackageInterner,
        symbols: &Interner,
        path: Vec<DefPathData>,
    ) -> LocalDefId {
        let data = path
            .into_iter()
            .map(|data| DisambiguatedDefPathData {
                data,
                disambiguator: 0,
            })
            .collect();
        let path = DefPath::new(package, data, packages, symbols).expect("path is valid");
        DefPathInterner::new(package, packages, symbols)
            .expect("path interner is valid")
            .intern(path)
            .expect("definition interns")
    }

    fn symbol(symbols: &Interner, name: &str) -> Symbol {
        symbols.intern(name).expect("symbol interns")
    }

    fn span(start: u32, end: u32) -> Span {
        Span {
            file: FileId::new(1),
            range: TextRange::new(TextSize::new(start), TextSize::new(end)).expect("ordered span"),
            ctxt: SpanCtxt::Root,
        }
    }

    fn source_span(start: u32, end: u32) -> SourceSpan {
        SourceSpan {
            file: FileId::new(1),
            range: TextRange::new(TextSize::new(start), TextSize::new(end))
                .expect("ordered source span"),
        }
    }

    fn wasi_io_view(packages: &PackageInterner, symbols: &Interner) -> WitView {
        let package_name = symbol(symbols, "io");
        let version = Version::parse("0.2.3").expect("pinned WASI version parses");
        let unit =
            WitUnit::wasi(package_name, symbols, &version).expect("WASI io unit is supported");
        let model = load_embedded_wasi_package(&unit, symbols)
            .expect("embedded WASI io package loads")
            .root()
            .clone();
        let view_package = packages
            .intern(unit.stable_key())
            .expect("WIT view package interns");
        let paths = Arc::new(
            DefPathInterner::new(view_package, packages, symbols)
                .expect("WIT view interner constructs"),
        );
        WitView::build(model, packages, symbols, paths).expect("WIT view builds")
    }

    fn module_scope(module_def_id: LocalDefId) -> Phase1ModuleScopes {
        Phase1ModuleScopes {
            module_def_id,
            declared: HashMap::new(),
            members: HashMap::new(),
            scope: HashMap::new(),
            imports: Vec::new(),
        }
    }

    fn binding(res: Res, visibility: sai_hir::Visibility, name_span: SourceSpan) -> Binding {
        Binding {
            res,
            name_span,
            origin: BindingOrigin::Decl,
            visibility,
        }
    }

    fn package_scopes(
        modules: Vec<Phase1ModuleScopes>,
        interfaces: HashMap<sai_hir::DefId, InterfaceScope>,
    ) -> Phase1PackageScopes {
        Phase1PackageScopes {
            modules,
            interfaces,
            exports: Default::default(),
            top_level_owners: Vec::new(),
            function_owners: Vec::new(),
            invariant_owners: Vec::new(),
            refinement_predicate_owners: Vec::new(),
            extern_function_owners: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn hir_id(owner: LocalDefId, local_id: u32) -> HirId {
        use sai_intern::Idx;
        HirId {
            owner: OwnerId { def_id: owner },
            local_id: ItemLocalId::from_raw(local_id),
        }
    }

    fn segment<'hir>(
        owner: LocalDefId,
        local_id: u32,
        sym: Symbol,
        start: u32,
        end: u32,
        args: Option<&'hir GenericArgs<'hir>>,
    ) -> PathSegment<'hir> {
        PathSegment {
            hir_id: hir_id(owner, local_id),
            name: Name {
                sym,
                span: span(start, end),
            },
            args,
        }
    }

    #[test]
    fn follows_nested_modules_and_consumes_every_segment() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let package = package_id(&packages, "acme:app#1.0");
        let nested_name = symbol(&symbols, "nested");
        let leaf_name = symbol(&symbols, "leaf");
        let root = local_def_id(package, &packages, &symbols, Vec::new());
        let nested = local_def_id(
            package,
            &packages,
            &symbols,
            vec![DefPathData::Module(nested_name)],
        );
        let leaf = local_def_id(
            package,
            &packages,
            &symbols,
            vec![
                DefPathData::Module(nested_name),
                DefPathData::ValueNs(leaf_name),
            ],
        );
        let mut root_scope = module_scope(root);
        root_scope.members.insert(
            nested_name,
            binding(
                Res::Def(DefKind::Module, nested.to_def_id()),
                sai_hir::Visibility::Private,
                source_span(1, 7),
            ),
        );
        let mut nested_scope = module_scope(nested);
        nested_scope.members.insert(
            leaf_name,
            binding(
                Res::Def(DefKind::Fn, leaf.to_def_id()),
                sai_hir::Visibility::Private,
                source_span(8, 12),
            ),
        );
        let scopes = package_scopes(vec![root_scope, nested_scope], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);
        let segments = [
            segment(root, 1, nested_name, 10, 16, None),
            segment(root, 2, leaf_name, 17, 21, None),
        ];

        let prefix = resolve_module_prefix(
            Res::Def(DefKind::Module, root.to_def_id()),
            &segments,
            package,
            &index,
        )
        .expect("nested module path resolves");

        assert_eq!(
            prefix,
            ModulePathPrefix {
                base: Res::Def(DefKind::Fn, leaf.to_def_id()),
                consumed_segments: 2,
                stop: ModulePathStop::SegmentsExhausted,
            }
        );
    }

    #[test]
    fn stops_before_a_segment_after_a_non_module_resolution() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let package = package_id(&packages, "acme:app#1.0");
        let root = local_def_id(package, &packages, &symbols, Vec::new());
        let function_name = symbol(&symbols, "run");
        let field_name = symbol(&symbols, "field");
        let function = local_def_id(
            package,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(function_name)],
        );
        let scopes = package_scopes(vec![module_scope(root)], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);
        let segments = [segment(root, 1, field_name, 5, 10, None)];

        let prefix = resolve_module_prefix(
            Res::Def(DefKind::Fn, function.to_def_id()),
            &segments,
            package,
            &index,
        )
        .expect("non-module suffix is left for the caller");

        assert_eq!(prefix.base, Res::Def(DefKind::Fn, function.to_def_id()));
        assert_eq!(prefix.consumed_segments, 0);
        assert_eq!(prefix.stop, ModulePathStop::NonModule);
    }

    #[test]
    fn missing_member_reports_the_reference_segment() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let package = package_id(&packages, "acme:app#1.0");
        let root = local_def_id(package, &packages, &symbols, Vec::new());
        let absent = symbol(&symbols, "absent");
        let scopes = package_scopes(vec![module_scope(root)], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);
        let segment_span = span(4, 10);
        let segments = [segment(root, 1, absent, 4, 10, None)];

        assert_eq!(
            resolve_module_prefix(
                Res::Def(DefKind::Module, root.to_def_id()),
                &segments,
                package,
                &index,
            ),
            Err(ModulePathError::MissingMember {
                segment_index: 0,
                name: absent,
                span: segment_span,
                target: ModuleTarget::from_res(Res::Def(DefKind::Module, root.to_def_id()))
                    .expect("module resolution has a target"),
            })
        );
    }

    #[test]
    fn inaccessible_member_reports_reference_and_declaration_spans() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let requester = package_id(&packages, "acme:app#1.0");
        let dependency = package_id(&packages, "acme:dependency#1.0");
        let root = local_def_id(dependency, &packages, &symbols, Vec::new());
        let hidden_name = symbol(&symbols, "hidden");
        let hidden = local_def_id(
            dependency,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(hidden_name)],
        );
        let declaration_span = source_span(30, 36);
        let mut root_scope = module_scope(root);
        root_scope.members.insert(
            hidden_name,
            binding(
                Res::Def(DefKind::Fn, hidden.to_def_id()),
                sai_hir::Visibility::Private,
                declaration_span,
            ),
        );
        let scopes = package_scopes(vec![root_scope], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);
        let reference_span = span(8, 14);
        let segments = [segment(root, 1, hidden_name, 8, 14, None)];

        assert_eq!(
            resolve_module_prefix(
                Res::Def(DefKind::Module, root.to_def_id()),
                &segments,
                requester,
                &index,
            ),
            Err(ModulePathError::InaccessibleMember {
                segment_index: 0,
                name: hidden_name,
                span: reference_span,
                declaration_span,
            })
        );
    }

    #[test]
    fn missing_module_scope_is_not_a_non_module_terminal() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let package = package_id(&packages, "acme:app#1.0");
        let root = local_def_id(package, &packages, &symbols, Vec::new());
        let member = symbol(&symbols, "member");
        let index = ModuleMemberIndex::new(std::iter::empty());
        let segments = [segment(root, 1, member, 0, 6, None)];

        assert_eq!(
            resolve_module_prefix(
                Res::Def(DefKind::Module, root.to_def_id()),
                &segments,
                package,
                &index,
            ),
            Err(ModulePathError::MissingTargetScope {
                segment_index: 0,
                target: root.to_def_id(),
            })
        );
    }

    #[test]
    fn error_resolutions_and_error_symbols_suppress_cascades() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let package = package_id(&packages, "acme:app#1.0");
        let root = local_def_id(package, &packages, &symbols, Vec::new());
        let name = symbol(&symbols, "name");
        let scopes = package_scopes(vec![module_scope(root)], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);
        let first_segment = segment(root, 1, name, 0, 4, None);

        let starting_error = resolve_module_prefix(Res::Err, &[first_segment], package, &index)
            .expect("existing error resolution is suppressed");
        assert_eq!(
            starting_error,
            ModulePathPrefix {
                base: Res::Err,
                consumed_segments: 0,
                stop: ModulePathStop::SuppressedError,
            }
        );

        let error_name = segment(root, 2, Symbol::ERROR, 5, 5, None);
        let poisoned_name = resolve_module_prefix(
            Res::Def(DefKind::Module, root.to_def_id()),
            &[error_name, first_segment],
            package,
            &index,
        )
        .expect("error symbol does not trigger lookup");
        assert_eq!(
            poisoned_name,
            ModulePathPrefix {
                base: Res::Err,
                consumed_segments: 1,
                stop: ModulePathStop::SuppressedError,
            }
        );
    }

    #[test]
    fn generic_arguments_are_not_consumed_or_modified() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let package = package_id(&packages, "acme:app#1.0");
        let root = local_def_id(package, &packages, &symbols, Vec::new());
        let module_name = symbol(&symbols, "nested");
        let nested = local_def_id(
            package,
            &packages,
            &symbols,
            vec![DefPathData::Module(module_name)],
        );
        let mut root_scope = module_scope(root);
        root_scope.members.insert(
            module_name,
            binding(
                Res::Def(DefKind::Module, nested.to_def_id()),
                sai_hir::Visibility::Private,
                source_span(0, 6),
            ),
        );
        let scopes = package_scopes(vec![root_scope, module_scope(nested)], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);
        let generic_args = GenericArgs {
            args: &[],
            span: span(6, 8),
        };
        let segments = [segment(root, 1, module_name, 0, 6, Some(&generic_args))];
        let original_args = segments[0].args.expect("arguments are present");

        let prefix = resolve_module_prefix(
            Res::Def(DefKind::Module, root.to_def_id()),
            &segments,
            package,
            &index,
        )
        .expect("module path resolves");

        assert_eq!(prefix.base, Res::Def(DefKind::Module, nested.to_def_id()));
        assert_eq!(prefix.consumed_segments, 1);
        assert!(std::ptr::eq(
            segments[0].args.expect("arguments remain present"),
            original_args
        ));
        assert_eq!(segments[0].args, Some(&generic_args));
    }

    #[test]
    fn resolves_wit_view_interface_member_as_its_definition() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let requester = package_id(&packages, "acme:app#1.0");
        let view = wasi_io_view(&packages, &symbols);
        let interface = view
            .interfaces
            .get("streams")
            .expect("WASI io streams interface exists");
        let (member_spelling, member) = interface
            .members
            .iter()
            .find(|(_, member)| member.kind == WitViewDefKind::Resource)
            .expect("streams exposes a resource type");
        let member_name = symbol(&symbols, member_spelling);
        let root = local_def_id(requester, &packages, &symbols, Vec::new());
        let index = ModuleMemberIndex::new_with_wit_views([], [&view], &symbols);

        let prefix = resolve_module_prefix(
            Res::Def(DefKind::Interface, interface.def),
            &[segment(root, 1, member_name, 0, 11, None)],
            requester,
            &index,
        )
        .expect("WIT resource member resolves");

        assert_eq!(
            prefix,
            ModulePathPrefix {
                base: Res::Def(DefKind::Resource, member.def),
                consumed_segments: 1,
                stop: ModulePathStop::SegmentsExhausted,
            }
        );
    }

    #[test]
    fn reports_missing_wit_view_interface_member() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let requester = package_id(&packages, "acme:app#1.0");
        let view = wasi_io_view(&packages, &symbols);
        let interface = view
            .interfaces
            .get("streams")
            .expect("WASI io streams interface exists");
        let missing_name = symbol(&symbols, "definitely_missing");
        let root = local_def_id(requester, &packages, &symbols, Vec::new());
        let index = ModuleMemberIndex::new_with_wit_views([], [&view], &symbols);

        assert!(matches!(
            resolve_module_prefix(
                Res::Def(DefKind::Interface, interface.def),
                &[segment(root, 1, missing_name, 4, 22, None)],
                requester,
                &index,
            ),
            Err(ModulePathError::MissingMember {
                name,
                segment_index: 0,
                ..
            }) if name == missing_name
        ));
    }
}
