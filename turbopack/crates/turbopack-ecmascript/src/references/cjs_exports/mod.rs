//! Removes the named exports of a statically-analyzable CommonJS module that no
//! consumer uses.
//!
//! A CommonJS module exposes its named exports as inline `exports.foo = …`
//! statements in the module body. This module:
//!  1. statically recognizes those named export writes ([`analyze_cjs_exports`]); and
//!  2. registers a [`CjsExportsDropCodeGen`] that, at code-gen time, removes the `exports.foo = …`
//!     statement for every export the module graph reports unused (and whose value is
//!     side-effect-free).
//!
//! An export is removed only when it is genuinely unobservable. The graph's
//! per-module usage (`compute_binding_usage_info`, read via
//! `ChunkingContext::module_export_usage`) reports `ExportUsage::All` for any
//! `require()` or otherwise dynamic consumer, so a statement is dropped only when
//! every consumer is a named ESM import that doesn't name it.

pub(crate) mod code_gen;
pub(crate) mod recognize;

pub use code_gen::CjsExportsDropCodeGen;
pub use recognize::analyze_cjs_exports;
