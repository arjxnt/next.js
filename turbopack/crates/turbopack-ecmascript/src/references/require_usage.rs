//! Determining how a `require("…")` result is consumed, so the `require`
//! reference can narrow which exports of the required module are used.
//!
//! [`analyze_require_usage`] classifies each call by how its result
//! flows: `const { a } = require(...)` / `require(...).foo` expose the members at
//! the call site; `const x = require(...)` binds the namespace, so every use of
//! `x` is scanned ([`NamespaceUsageVisitor`]). Deny-by-default: any use that
//! isn't a static member read yields `ExportUsage::All`.

use rustc_hash::{FxHashMap, FxHashSet};
use swc_core::{
    common::{BytePos, Mark},
    ecma::{
        ast::{
            CallExpr, Callee, ComputedPropName, Expr, Id, Ident, Lit, MemberExpr, MemberProp, Pat,
            Program, VarDeclarator,
        },
        visit::{Visit, VisitWith, noop_visit_type},
    },
};
use turbo_rcstr::RcStr;
use turbo_tasks::FxIndexSet;
use turbopack_core::resolve::ExportUsage;

use crate::{
    analyzer::{
        graph::visitor::{extract_name_from_member_prop, extract_names_from_object_pat},
        side_effects::is_global,
    },
    utils::unparen,
};

/// For each literal-string `require("…")` call, the [`ExportUsage`] implied by
/// how its result is consumed, keyed by the call's source position. Calls not
/// present fall back to the reference default (`All`).
pub fn analyze_require_usage(
    program: &Program,
    unresolved_mark: Mark,
) -> FxHashMap<BytePos, ExportUsage> {
    let mut collector = RequireBindingCollector {
        unresolved_mark,
        bindings: FxHashMap::default(),
        resolved: FxHashMap::default(),
    };
    program.visit_with(&mut collector);

    let mut result = collector.resolved;

    // Second pass: for `const x = require(...)` bindings, scan every use of `x`.
    if !collector.bindings.is_empty() {
        let mut visitor = NamespaceUsageVisitor {
            tracked: collector.bindings.keys().cloned().collect(),
            usage: collector
                .bindings
                .keys()
                .map(|id| (id.clone(), NamespaceUsage::Members(FxIndexSet::default())))
                .collect(),
        };
        program.visit_with(&mut visitor);

        for (id, span_lo) in collector.bindings {
            let usage = match visitor.usage.remove(&id) {
                Some(NamespaceUsage::Members(names)) => {
                    ExportUsage::PartialNamespaceObject(names.into_iter().collect())
                }
                Some(NamespaceUsage::Escaped) | None => ExportUsage::All,
            };
            result.insert(span_lo, usage);
        }
    }

    result
}

/// If `expr` is a `require("<string literal>")` call, returns it.
fn as_require_call(expr: &Expr, unresolved_mark: Mark) -> Option<&CallExpr> {
    let Expr::Call(call) = unparen(expr) else {
        return None;
    };
    let Callee::Expr(callee) = &call.callee else {
        return None;
    };
    let Expr::Ident(f) = &**callee else {
        return None;
    };
    if !is_global(f, "require", unresolved_mark) {
        return None;
    }
    let [arg] = &call.args[..] else {
        return None;
    };
    if arg.spread.is_some() || !matches!(unparen(&arg.expr), Expr::Lit(Lit::Str(_))) {
        return None;
    }
    Some(call)
}

/// Classifies each `require("<literal>")` call from its immediate syntactic
/// position: `const { a } = require(...)` and `require(...).foo` are resolved
/// here (`resolved`); `const x = require(...)` records the binding's [`Id`] and
/// call position for the whole-module scan (`bindings` → [`NamespaceUsageVisitor`]).
struct RequireBindingCollector {
    unresolved_mark: Mark,
    bindings: FxHashMap<Id, BytePos>,
    resolved: FxHashMap<BytePos, ExportUsage>,
}

impl Visit for RequireBindingCollector {
    noop_visit_type!();

    fn visit_var_declarator(&mut self, n: &VarDeclarator) {
        if let Some(init) = &n.init
            && let Some(call) = as_require_call(init, self.unresolved_mark)
        {
            match &n.name {
                // `const x = require(...)`: we will need to analyze how `x` is used module-wide.
                Pat::Ident(binding) => {
                    self.bindings.insert(binding.id.to_id(), call.span.lo);
                }
                // `const { a, b } = require(...)`: the used members are the keys.
                Pat::Object(_) => {
                    let usage = match extract_names_from_object_pat(&n.name) {
                        Some(names) => ExportUsage::PartialNamespaceObject(names),
                        // Rest / computed key → the whole namespace is needed.
                        None => ExportUsage::All,
                    };
                    self.resolved.insert(call.span.lo, usage);
                }
                // `const [a] = require(...)` and other patterns need the whole value.
                _ => {
                    self.resolved.insert(call.span.lo, ExportUsage::All);
                }
            }
        }
        n.visit_children_with(self);
    }

    fn visit_member_expr(&mut self, n: &MemberExpr) {
        if let Some(call) = as_require_call(&n.obj, self.unresolved_mark) {
            // `require(...).foo`: only that member is used.
            let usage = match extract_name_from_member_prop(&n.prop) {
                Some(names) => ExportUsage::PartialNamespaceObject(names),
                None => ExportUsage::All,
            };
            self.resolved.insert(call.span.lo, usage);
        }
        n.visit_children_with(self);
    }
}

/// How a namespace-valued binding (a `require()` result) is used module-wide.
#[derive(Debug)]
enum NamespaceUsage {
    /// Members read via static access (`ns.foo` / `ns["foo"]`), in source order.
    /// Empty means the binding is never used.
    Members(FxIndexSet<RcStr>),
    /// Used wholesale at least once (bare reference, reassignment, dynamic
    /// member, spread, …), so the whole namespace is observable.
    Escaped,
}

struct NamespaceUsageVisitor {
    tracked: FxHashSet<Id>,
    usage: FxHashMap<Id, NamespaceUsage>,
}

impl NamespaceUsageVisitor {
    fn escape(&mut self, id: &Id) {
        if let Some(usage) = self.usage.get_mut(id) {
            *usage = NamespaceUsage::Escaped;
        }
    }

    fn record_member(&mut self, id: &Id, name: RcStr) {
        if let Some(NamespaceUsage::Members(names)) = self.usage.get_mut(id) {
            names.insert(name);
        }
    }
}

impl Visit for NamespaceUsageVisitor {
    noop_visit_type!();

    // A tracked ident not consumed as a static member read (below) or skipped as
    // a declaration is a wholesale use.
    fn visit_ident(&mut self, n: &Ident) {
        let id = n.to_id();
        if self.tracked.contains(&id) {
            self.escape(&id);
        }
    }

    fn visit_member_expr(&mut self, n: &MemberExpr) {
        if let Expr::Ident(obj) = &*n.obj {
            let id = obj.to_id();
            if self.tracked.contains(&id) {
                match extract_name_from_member_prop(&n.prop) {
                    Some(names) => {
                        for name in names {
                            self.record_member(&id, name);
                        }
                    }
                    // `ns[dynamic]` / private member — not statically known, so
                    // the whole namespace is observable.
                    None => {
                        self.escape(&id);
                        if let MemberProp::Computed(ComputedPropName { expr, .. }) = &n.prop {
                            expr.visit_with(self);
                        }
                    }
                }
                // Consumed here; don't let `visit_ident` treat it as wholesale.
                return;
            }
        }
        n.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, n: &VarDeclarator) {
        // Don't treat a tracked binding's own declaration as a use.
        if let Pat::Ident(binding) = &n.name
            && self.tracked.contains(&binding.id.to_id())
        {
            if let Some(init) = &n.init {
                init.visit_with(self);
            }
            return;
        }
        n.visit_children_with(self);
    }
}
