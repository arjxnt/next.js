//! Static recognition of a CommonJS module's named `exports.foo = …` writes.

use swc_core::{
    common::{BytePos, Mark, SyntaxContext},
    ecma::{
        ast::{
            AssignOp, AssignTarget, Expr, ExprStmt, Lit, MemberProp, Program, SimpleAssignTarget,
            Stmt,
        },
        utils::{ExprCtx, ExprExt},
        visit::{Visit, VisitWith, noop_visit_type},
    },
};
use turbo_rcstr::RcStr;
use turbo_tasks::FxIndexSet;

use crate::{
    analyzer::side_effects::{is_global, is_module_dot_exports},
    utils::unparen,
};

/// The statically-determined named exports of a CommonJS module, produced by
/// [`analyze_cjs_exports`].
#[derive(Debug, Default)]
pub struct CommonJsExportsAnalysis {
    /// Statically known export names, in source order.
    pub names: FxIndexSet<RcStr>,
    /// Whether the transpiled-ESM `__esModule` marker is set.
    pub has_es_module: bool,
    /// Whether the module touches `exports` / `module` outside the recognized
    /// safe forms. When set, `names` may be incomplete and the module must be
    /// treated as opaque CommonJS (no exports dropped).
    pub is_unsafe: bool,
    /// Side-effect-free `exports.NAME = …` writes that may be dropped if unused,
    /// each carrying its statement's source position for the code-gen.
    pub droppable: Vec<DroppableExport>,
}

/// A single `exports.NAME = <pure expr>` top-level statement that may be removed
/// if `NAME` is unused.
#[derive(Debug)]
pub struct DroppableExport {
    pub name: RcStr,
    /// Source position (`Stmt::span().lo`) of the export-write statement.
    pub span_lo: BytePos,
}

/// Recognized safe forms, as top-level expression statements only:
/// - `exports.NAME = …` / `module.exports.NAME = …` with a plain identifier name
/// - the `__esModule` interop marker (`exports.__esModule = true` / `module.exports.__esModule =
///   true`)
///
/// Anything else touching `exports` / `module` — aliasing, escapes, computed
/// keys, `module.exports = …` reassignment, top-level `this`, or an export write
/// in any other position (nested in a function, behind `if`, inside another
/// expression) — sets `is_unsafe`.
///
/// TODO (@sampoder): handle `Object.defineProperty(exports, "__esModule", { value: true })`
pub fn analyze_cjs_exports(program: &Program, unresolved_mark: Mark) -> CommonJsExportsAnalysis {
    let mut analysis = CommonJsExportsAnalysis::default();
    let mut visitor = ExportsTaintVisitor {
        unresolved_mark,
        fn_depth: 0,
        tainted: false,
    };
    let expr_ctx = ExprCtx {
        unresolved_ctxt: SyntaxContext::empty().apply_mark(unresolved_mark),
        is_unresolved_ref_safe: false,
        in_strict: false,
        remaining_depth: 4,
    };

    let statements: Vec<&Stmt> = match program {
        Program::Script(script) => script.body.iter().collect(),
        Program::Module(module) => module.body.iter().filter_map(|i| i.as_stmt()).collect(),
    };

    for stmt in statements {
        if analysis.is_unsafe || visitor.tainted {
            break;
        }
        let Stmt::Expr(ExprStmt { expr, span }) = stmt else {
            stmt.visit_with(&mut visitor);
            continue;
        };
        // A comma expression (`exports.a = 1, exports.b = 2`) holds multiple
        // writes; `unparen` sees only the last operand, so bail for now.
        // TODO (@sampoder): improve support for sequences.
        if matches!(strip_parens(expr), Expr::Seq(_)) {
            analysis.is_unsafe = true;
            break;
        }
        let Expr::Assign(assign) = unparen(expr) else {
            stmt.visit_with(&mut visitor);
            continue;
        };
        if assign.op != AssignOp::Assign
            || !matches!(
                &assign.left,
                AssignTarget::Simple(SimpleAssignTarget::Member(_))
            )
        {
            stmt.visit_with(&mut visitor);
            continue;
        }
        let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left else {
            unreachable!()
        };

        if is_module_dot_exports(member, unresolved_mark) {
            // `module.exports = …` reassignment: not supported (object-literal
            // decomposition is a follow-up). Treat as opaque.
            analysis.is_unsafe = true;
            break;
        }
        if !is_exports_object(&member.obj, unresolved_mark) {
            // A write to some other object, e.g. `foo.bar = 1`.
            stmt.visit_with(&mut visitor);
            continue;
        }
        let MemberProp::Ident(name) = &member.prop else {
            // Computed / non-identifier export key, e.g. `exports[key] = …`.
            analysis.is_unsafe = true;
            break;
        };

        if name.sym.as_ref() == "__esModule" {
            if matches!(unparen(&assign.right), Expr::Lit(Lit::Bool(b)) if b.value) {
                // The `__esModule` interop marker. Flag it via `has_es_module`
                // for the code-gen's interop guard, but keep it out of
                // `names`/`droppable`: it's read by ESM interop at runtime, never
                // imported by name, so it must not be treated as a droppable
                // export.
                analysis.has_es_module = true;
            } else {
                // `exports.__esModule = 0` / `= someVar`.
                analysis.is_unsafe = true;
                break;
            }
            continue;
        }

        // `exports.NAME = …` / `module.exports.NAME = …`.
        let name = RcStr::from(name.sym.as_str());
        // A second top-level write to the same name is ambiguous to drop — bail.
        if !analysis.names.insert(name.clone()) {
            analysis.is_unsafe = true;
            break;
        }
        // The RHS may still leak the exports object (`exports.a = [exports]`);
        // the write's own target is the recognized form itself.
        assign.right.visit_with(&mut visitor);
        // Only side-effect-free values are droppable: removing the statement
        // removes the evaluation of its RHS, so an impure RHS must be kept.
        if !assign.right.may_have_side_effects(expr_ctx) {
            analysis.droppable.push(DroppableExport {
                name,
                span_lo: span.lo,
            });
        }
    }

    analysis.is_unsafe |= visitor.tainted;
    if analysis.is_unsafe {
        analysis.names.clear();
        analysis.droppable.clear();
        analysis.has_es_module = false;
    }
    analysis
}

fn strip_parens(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(paren) => strip_parens(&paren.expr),
        _ => expr,
    }
}

/// Whether `expr` is the real (unshadowed) `exports` or `module.exports`.
fn is_exports_object(expr: &Expr, unresolved_mark: Mark) -> bool {
    match unparen(expr) {
        Expr::Ident(o) => is_global(o, "exports", unresolved_mark),
        Expr::Member(inner) => is_module_dot_exports(inner, unresolved_mark),
        _ => false,
    }
}

/// Flags any reference to the real `exports` / `module` bindings, and any
/// top-level `this` (which aliases `exports`). Recognized export writes never
/// reach this visitor via their target; everything else in the module does —
/// including export writes in unliftable positions, whose `exports` root lands
/// here.
struct ExportsTaintVisitor {
    unresolved_mark: Mark,
    /// Function nesting depth; `0` is module top level, where `this` aliases
    /// `exports`.
    fn_depth: u32,
    tainted: bool,
}

impl Visit for ExportsTaintVisitor {
    noop_visit_type!();

    fn visit_stmt(&mut self, n: &Stmt) {
        if self.tainted {
            return;
        }
        n.visit_children_with(self);
    }

    fn visit_expr(&mut self, n: &Expr) {
        if self.tainted {
            return;
        }
        n.visit_children_with(self);
    }

    fn visit_function(&mut self, n: &swc_core::ecma::ast::Function) {
        self.fn_depth += 1;
        n.visit_children_with(self);
        self.fn_depth -= 1;
    }

    // Arrow functions inherit `this` lexically instead of rebinding it, so a
    // `this` inside a top-level arrow still aliases `exports`. They must NOT
    // increment `fn_depth` (unlike `function`/constructor, which rebind `this`) —
    // just recurse.
    fn visit_arrow_expr(&mut self, n: &swc_core::ecma::ast::ArrowExpr) {
        n.visit_children_with(self);
    }

    fn visit_constructor(&mut self, n: &swc_core::ecma::ast::Constructor) {
        self.fn_depth += 1;
        n.visit_children_with(self);
        self.fn_depth -= 1;
    }

    fn visit_this_expr(&mut self, _: &swc_core::ecma::ast::ThisExpr) {
        if self.fn_depth == 0 {
            self.tainted = true;
        }
    }

    fn visit_ident(&mut self, i: &swc_core::ecma::ast::Ident) {
        if is_global(i, "exports", self.unresolved_mark)
            || is_global(i, "module", self.unresolved_mark)
        {
            self.tainted = true;
        }
    }
}
