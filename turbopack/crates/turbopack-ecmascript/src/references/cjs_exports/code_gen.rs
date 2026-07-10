//! The code-gen that removes a static CommonJS module's unused named exports.

use anyhow::Result;
use bincode::{Decode, Encode};
use rustc_hash::FxHashSet;
use swc_core::{
    common::{BytePos, DUMMY_SP, Spanned},
    ecma::ast::{EmptyStmt, ModuleItem, Program, Stmt},
};
use turbo_rcstr::{RcStr, rcstr};
use turbo_tasks::{NonLocalValue, ResolvedVc, Vc, debug::ValueDebugFormat, trace::TraceRawVcs};
use turbopack_core::{
    chunk::ChunkingContext, module_graph::binding_usage_info::ModuleExportUsageInfo,
};

use crate::{
    chunk::{EcmascriptChunkPlaceable, EcmascriptExports},
    code_gen::{AstModifier, CodeGen, CodeGeneration},
    references::cjs_exports::recognize::CommonJsExportsAnalysis,
};

/// Removes the `exports.NAME = …` statement for every named CommonJS export the
/// module graph proved unused. Registered only for statically-analyzable
/// CommonJS modules (see [`super::recognize::analyze_cjs_exports`]).
#[derive(
    PartialEq, Eq, TraceRawVcs, ValueDebugFormat, NonLocalValue, Hash, Debug, Encode, Decode,
)]
pub struct CjsExportsDropCodeGen {
    drops: Vec<CjsExportDrop>,
    /// Whether the module sets the `__esModule` interop marker. A default import
    /// of a module *without* it receives the whole `module.exports` object via
    /// interop, so nothing may be dropped in that case (see `code_generation`).
    has_es_module: bool,
}

#[derive(
    PartialEq, Eq, TraceRawVcs, ValueDebugFormat, NonLocalValue, Hash, Debug, Encode, Decode,
)]
struct CjsExportDrop {
    name: RcStr,
    /// Source position (`Stmt::span().lo`) of the `exports.NAME = …` statement,
    /// used to locate it in the module body at code-gen time.
    span_lo: u32,
}

impl CjsExportsDropCodeGen {
    /// Builds a drop code-gen from a completed [`CommonJsExportsAnalysis`].
    /// Returns `None` when nothing is droppable.
    pub fn new(analysis: &CommonJsExportsAnalysis) -> Option<Self> {
        if analysis.is_unsafe || analysis.droppable.is_empty() {
            return None;
        }
        Some(CjsExportsDropCodeGen {
            drops: analysis
                .droppable
                .iter()
                .map(|d| CjsExportDrop {
                    name: d.name.clone(),
                    span_lo: d.span_lo.0,
                })
                .collect(),
            has_es_module: analysis.has_es_module,
        })
    }

    pub async fn code_generation(
        &self,
        chunking_context: Vc<Box<dyn ChunkingContext>>,
        module: ResolvedVc<Box<dyn EcmascriptChunkPlaceable>>,
        _exports: ResolvedVc<EcmascriptExports>,
    ) -> Result<CodeGeneration> {
        let export_usage_info = chunking_context
            .module_export_usage(*ResolvedVc::upcast(module))
            .await?;
        let export_usage_info = export_usage_info.export_usage.await?;

        // `All` means the module is (or may be) consumed wholesale — e.g. via
        // `require()` or a namespace import — so nothing can be dropped.
        if matches!(*export_usage_info, ModuleExportUsageInfo::All) {
            return Ok(CodeGeneration::empty());
        }

        // Without `__esModule`, `import x from './cjs'` binds `x` to the entire
        // `module.exports` object (CJS interop provides no separate `default`),
        // so any named export can be read as `x.NAME`. Seeing `default` used
        // therefore means every named export is reachable — drop nothing.
        if !self.has_es_module && export_usage_info.is_export_used(&rcstr!("default")) {
            return Ok(CodeGeneration::empty());
        }

        let unused: FxHashSet<BytePos> = self
            .drops
            .iter()
            .filter(|d| !export_usage_info.is_export_used(&d.name))
            .map(|d| BytePos(d.span_lo))
            .collect();

        if unused.is_empty() {
            return Ok(CodeGeneration::empty());
        }

        // Blank each unused export write, matched by source position among the
        // module's top-level statements.
        Ok(CodeGeneration::visitors(vec![(
            Vec::new(),
            Box::new(DropExportWrites { unused }),
        )]))
    }
}

/// Blanks the top-level `exports.NAME = …` statements whose source position is in
/// `unused`, replacing each with an empty statement.
struct DropExportWrites {
    unused: FxHashSet<BytePos>,
}

impl DropExportWrites {
    fn drop_matching(&self, stmt: &mut Stmt) {
        if self.unused.contains(&stmt.span().lo) {
            *stmt = Stmt::Empty(EmptyStmt { span: DUMMY_SP });
        }
    }
}

impl AstModifier for DropExportWrites {
    fn visit_mut_program(&self, program: &mut Program) {
        match program {
            Program::Module(module) => {
                for item in &mut module.body {
                    if let ModuleItem::Stmt(stmt) = item {
                        self.drop_matching(stmt);
                    }
                }
            }
            Program::Script(script) => {
                for stmt in &mut script.body {
                    self.drop_matching(stmt);
                }
            }
        }
    }
}

impl From<CjsExportsDropCodeGen> for CodeGen {
    fn from(val: CjsExportsDropCodeGen) -> Self {
        CodeGen::CjsExportsDropCodeGen(val)
    }
}
