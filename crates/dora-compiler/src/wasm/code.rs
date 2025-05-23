use crate::backend::AtomicBinOp;
use crate::backend::AtomicOrdering;
use crate::backend::IntCC;
use crate::backend::LoadStoreOptions;
use crate::context::Context;
use crate::conversion::builder::OpBuilder;
use crate::errors::Result;
use crate::intrinsics::{is_f32_arithmetic, is_f64_arithmetic};
use crate::value::ToContextValue;
use crate::wasm::backend::trap_float_if_not_representable_as_int;
use crate::wasm::intrinsics::MemoryCache;

use super::backend::{WASMBackend, is_zero, trap, trap_call};
use super::func::FuncTranslator;
use super::intrinsics::CtxType;
use super::intrinsics::FunctionCache;
use super::intrinsics::GlobalCache;
use super::ty::{type_to_mlir, type_to_mlir_zero_attribute};
use crate::errors::CompileError;
use crate::state::ControlFrame;
use crate::state::ExtraInfo;
use crate::state::IfElseState;
use crate::state::PhiValue;
use crate::wasm::meter::op_gas_cost;
use dora_runtime::symbols;
use dora_runtime::wasm::trap::TrapCode;
use melior::dialect::ods;
use melior::dialect::{arith, cf, func, llvm, scf};
use melior::ir::attribute::DenseI64ArrayAttribute;
use melior::ir::attribute::FlatSymbolRefAttribute;
use melior::ir::attribute::StringAttribute;
use melior::ir::{Block, BlockRef, Location, Region, Type, Value, ValueLike};
use smallvec::SmallVec;
use wasmer::WASM_PAGE_SIZE;
use wasmer::wasmparser::MemArg;
use wasmer_compiler::from_binaryreadererror_wasmerror;
use wasmer_compiler::types::symbols::SymbolRegistry;
use wasmer_compiler::wasmparser::Operator;
use wasmer_compiler::wptype_to_type;
use wasmer_compiler::{ModuleTranslationState, wpheaptype_to_type};
use wasmer_types::entity::PrimaryMap;
use wasmer_types::{
    FunctionIndex, FunctionType, GlobalIndex, MemoryIndex, MemoryStyle, ModuleInfo, SignatureIndex,
    TableIndex, TableStyle, WasmResult,
};

macro_rules! op {
    ($builder:ident, $state:ident, $op:ident, i32) => {
        let (input, _) = $state.pop1_extra()?;
        let result = $builder
            .create(
                dora_ir::wasm::$op(
                    $builder.context(),
                    $builder.i32_ty(),
                    input,
                    $builder.unknown_loc(),
                )
                .into(),
            )
            .result(0)
            .map_err(|e| CompileError::Codegen(e.to_string()))?
            .to_ctx_value();
        $state.push1(result);
    };
    ($builder:ident, $state:ident, $op:ident, i64) => {
        let (input, _) = $state.pop1_extra()?;
        let result = $builder
            .create(
                dora_ir::wasm::$op(
                    $builder.context(),
                    $builder.i64_ty(),
                    input,
                    $builder.unknown_loc(),
                )
                .into(),
            )
            .result(0)
            .map_err(|e| CompileError::Codegen(e.to_string()))?
            .to_ctx_value();
        $state.push1(result);
    };
    ($builder:ident, $state:ident, $op:ident, i64 => i32) => {
        let (input, _) = $state.pop1_extra()?;
        let result = $builder
            .create(
                dora_ir::wasm::$op(
                    $builder.context(),
                    $builder.i32_ty(),
                    input,
                    $builder.unknown_loc(),
                )
                .into(),
            )
            .result(0)
            .map_err(|e| CompileError::Codegen(e.to_string()))?
            .to_ctx_value();
        $state.push1(result);
    };
    ($builder:ident, $state:ident, $op:ident, f32) => {
        let (input, _) = $state.pop1_extra()?;
        let result = $builder
            .create(
                dora_ir::wasm::$op(
                    $builder.context(),
                    $builder.intrinsics.f32_ty,
                    input,
                    $builder.unknown_loc(),
                )
                .into(),
            )
            .result(0)
            .map_err(|e| CompileError::Codegen(e.to_string()))?
            .to_ctx_value();
        $state.push1(result);
    };
    ($builder:ident, $state:ident, $op:ident, f64) => {
        let (input, _) = $state.pop1_extra()?;
        let result = $builder
            .create(
                dora_ir::wasm::$op(
                    $builder.context(),
                    $builder.intrinsics.f64_ty,
                    input,
                    $builder.unknown_loc(),
                )
                .into(),
            )
            .result(0)
            .map_err(|e| CompileError::Codegen(e.to_string()))?
            .to_ctx_value();
        $state.push1(result);
    };
}

macro_rules! bin_op {
    ($builder:ident, $state:ident, $op:ident, i32) => {
        let ((v1, _), (v2, _)) = $state.pop2_extra()?;
        let result = $builder
            .create(
                dora_ir::wasm::$op(
                    $builder.context(),
                    $builder.i32_ty(),
                    v1,
                    v2,
                    $builder.unknown_loc(),
                )
                .into(),
            )
            .result(0)
            .map_err(|e| CompileError::Codegen(e.to_string()))?
            .to_ctx_value();
        $state.push1(result);
    };
    ($builder:ident, $state:ident, $op:ident, i64) => {
        let ((v1, _), (v2, _)) = $state.pop2_extra()?;
        let result = $builder
            .create(
                dora_ir::wasm::$op(
                    $builder.context(),
                    $builder.i64_ty(),
                    v1,
                    v2,
                    $builder.unknown_loc(),
                )
                .into(),
            )
            .result(0)
            .map_err(|e| CompileError::Codegen(e.to_string()))?
            .to_ctx_value();
        $state.push1(result);
    };
    ($builder:ident, $state:ident, $op:ident, i64 => i32) => {
        let ((v1, _), (v2, _)) = $state.pop2_extra()?;
        let result = $builder
            .create(
                dora_ir::wasm::$op(
                    $builder.context(),
                    $builder.i32_ty(),
                    v1,
                    v2,
                    $builder.unknown_loc(),
                )
                .into(),
            )
            .result(0)
            .map_err(|e| CompileError::Codegen(e.to_string()))?
            .to_ctx_value();
        $state.push1(result);
    };
    ($builder:ident, $state:ident, $op:ident, f32) => {
        let ((v1, _), (v2, _)) = $state.pop2_extra()?;
        let result = $builder
            .create(
                dora_ir::wasm::$op(
                    $builder.context(),
                    $builder.intrinsics.f32_ty,
                    v1,
                    v2,
                    $builder.unknown_loc(),
                )
                .into(),
            )
            .result(0)
            .map_err(|e| CompileError::Codegen(e.to_string()))?
            .to_ctx_value();
        $state.push1(result);
    };
    ($builder:ident, $state:ident, $op:ident, f32 => i32) => {
        let ((v1, _), (v2, _)) = $state.pop2_extra()?;
        let result = $builder
            .create(
                dora_ir::wasm::$op(
                    $builder.context(),
                    $builder.i32_ty(),
                    v1,
                    v2,
                    $builder.unknown_loc(),
                )
                .into(),
            )
            .result(0)
            .map_err(|e| CompileError::Codegen(e.to_string()))?
            .to_ctx_value();
        $state.push1(result);
    };
    ($builder:ident, $state:ident, $op:ident, f64) => {
        let ((v1, _), (v2, _)) = $state.pop2_extra()?;
        let result = $builder
            .create(
                dora_ir::wasm::$op(
                    $builder.context(),
                    $builder.intrinsics.f64_ty,
                    v1,
                    v2,
                    $builder.unknown_loc(),
                )
                .into(),
            )
            .result(0)
            .map_err(|e| CompileError::Codegen(e.to_string()))?
            .to_ctx_value();
        $state.push1(result);
    };
    ($builder:ident, $state:ident, $op:ident, f64 => i32) => {
        let ((v1, _), (v2, _)) = $state.pop2_extra()?;
        let result = $builder
            .create(
                dora_ir::wasm::$op(
                    $builder.context(),
                    $builder.i32_ty(),
                    v1,
                    v2,
                    $builder.unknown_loc(),
                )
                .into(),
            )
            .result(0)
            .map_err(|e| CompileError::Codegen(e.to_string()))?
            .to_ctx_value();
        $state.push1(result);
    };
}
macro_rules! atomicrmw_op {
    ($builder:ident, $state:ident, $memarg:ident, $fcx:ident, $backend:ident, $region:ident, $block:ident, $op:ident, i32) => {
        let (offset, value) = $state.pop2()?;
        let memory_index = MemoryIndex::from_u32(0);
        let (block, effective_address) = Self::resolve_memory_ptr(
            memory_index,
            $memarg,
            offset,
            4,
            $fcx,
            $backend.ctx,
            $region,
            $block,
        )?;
        let builder = OpBuilder::new_with_block($builder.context(), block);
        let result = builder.make(
            ods::llvm::atomicrmw(
                builder.context(),
                builder.i32_ty(),
                effective_address,
                value,
                StringAttribute::new(builder.context(), AtomicBinOp::$op.attr_str().as_str())
                    .into(),
                StringAttribute::new(
                    $builder.context(),
                    AtomicOrdering::SequentiallyConsistent.attr_str().as_str(),
                )
                .into(),
                builder.get_insert_location(),
            )
            .into(),
        )?;
        $state.push1(result.to_ctx_value());
        return Ok(block);
    };
    ($builder:ident, $state:ident, $memarg:ident, $fcx:ident, $backend:ident, $region:ident, $block:ident, $op:ident, i32, i8) => {
        let (offset, value) = $state.pop2()?;
        let memory_index = MemoryIndex::from_u32(0);
        let (block, effective_address) = Self::resolve_memory_ptr(
            memory_index,
            $memarg,
            offset,
            size_of::<i8>(),
            $fcx,
            $backend.ctx,
            $region,
            $block,
        )?;
        let builder = OpBuilder::new_with_block($builder.context(), block);
        let value = builder.make(arith::trunci(
            value,
            $builder.i8_ty(),
            $builder.get_insert_location(),
        ))?;
        let result = builder.make(
            ods::llvm::atomicrmw(
                builder.context(),
                builder.i8_ty(),
                effective_address,
                value,
                StringAttribute::new(builder.context(), AtomicBinOp::$op.attr_str().as_str())
                    .into(),
                StringAttribute::new(
                    builder.context(),
                    AtomicOrdering::SequentiallyConsistent.attr_str().as_str(),
                )
                .into(),
                builder.get_insert_location(),
            )
            .into(),
        )?;
        let result = builder.make(arith::extui(
            result,
            builder.i32_ty(),
            builder.get_insert_location(),
        ))?;
        $state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f32());
        return Ok(block);
    };
    ($builder:ident, $state:ident, $memarg:ident, $fcx:ident, $backend:ident, $region:ident, $block:ident, $op:ident, i32, i16) => {
        let (offset, value) = $state.pop2()?;
        let memory_index = MemoryIndex::from_u32(0);
        let (block, effective_address) = Self::resolve_memory_ptr(
            memory_index,
            $memarg,
            offset,
            size_of::<i16>(),
            $fcx,
            $backend.ctx,
            $region,
            $block,
        )?;
        let builder = OpBuilder::new_with_block($builder.context(), block);
        let value = builder.make(arith::trunci(
            value,
            $builder.i16_ty(),
            $builder.get_insert_location(),
        ))?;
        let result = builder.make(
            ods::llvm::atomicrmw(
                builder.context(),
                builder.i16_ty(),
                effective_address,
                value,
                StringAttribute::new(builder.context(), AtomicBinOp::$op.attr_str().as_str())
                    .into(),
                StringAttribute::new(
                    builder.context(),
                    AtomicOrdering::SequentiallyConsistent.attr_str().as_str(),
                )
                .into(),
                builder.get_insert_location(),
            )
            .into(),
        )?;
        let result = builder.make(arith::extui(
            result,
            builder.i32_ty(),
            builder.get_insert_location(),
        ))?;
        $state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f32());
        return Ok(block);
    };
    ($builder:ident, $state:ident, $memarg:ident, $fcx:ident, $backend:ident, $region:ident, $block:ident, $op:ident, i64) => {
        let (offset, value) = $state.pop2()?;
        let memory_index = MemoryIndex::from_u32(0);
        let (block, effective_address) = Self::resolve_memory_ptr(
            memory_index,
            $memarg,
            offset,
            size_of::<i64>(),
            $fcx,
            $backend.ctx,
            $region,
            $block,
        )?;
        let builder = OpBuilder::new_with_block($builder.context(), block);
        let result = builder.make(
            ods::llvm::atomicrmw(
                builder.context(),
                builder.i64_ty(),
                effective_address,
                value,
                StringAttribute::new(builder.context(), AtomicBinOp::$op.attr_str().as_str())
                    .into(),
                StringAttribute::new(
                    builder.context(),
                    AtomicOrdering::SequentiallyConsistent.attr_str().as_str(),
                )
                .into(),
                builder.get_insert_location(),
            )
            .into(),
        )?;
        $state.push1(result.to_ctx_value());
        return Ok(block);
    };
    ($builder:ident, $state:ident, $memarg:ident, $fcx:ident, $backend:ident, $region:ident, $block:ident, $op:ident, i64, $ty:ident) => {
        let (offset, value) = $state.pop2()?;
        let memory_index = MemoryIndex::from_u32(0);
        let (block, effective_address) = Self::resolve_memory_ptr(
            memory_index,
            $memarg,
            offset,
            size_of::<i8>(),
            $fcx,
            $backend.ctx,
            $region,
            $block,
        )?;
        let builder = OpBuilder::new_with_block($builder.context(), block);
        let value = builder.make(arith::trunci(
            value,
            builder.i8_ty(),
            builder.get_insert_location(),
        ))?;
        let result = $builder.make(
            ods::llvm::atomicrmw(
                builder.context(),
                builder.i8_ty(),
                effective_address,
                value,
                StringAttribute::new(builder.context(), AtomicBinOp::$op.attr_str().as_str())
                    .into(),
                StringAttribute::new(
                    $builder.context(),
                    AtomicOrdering::SequentiallyConsistent.attr_str().as_str(),
                )
                .into(),
                builder.get_insert_location(),
            )
            .into(),
        )?;
        let result = builder.make(arith::extui(
            result,
            builder.i64_ty(),
            builder.get_insert_location(),
        ))?;
        $state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f64());
        return Ok(block);
    };
    ($builder:ident, $state:ident, $memarg:ident, $fcx:ident, $backend:ident, $region:ident, $block:ident, $op:ident, i64, i16) => {
        let (offset, value) = $state.pop2()?;
        let memory_index = MemoryIndex::from_u32(0);
        let (block, effective_address) = Self::resolve_memory_ptr(
            memory_index,
            $memarg,
            offset,
            size_of::<i16>(),
            $fcx,
            $backend.ctx,
            $region,
            $block,
        )?;
        let builder = OpBuilder::new_with_block($builder.context(), block);
        let value = builder.make(arith::trunci(
            value,
            builder.i16_ty(),
            builder.get_insert_location(),
        ))?;
        let result = $builder.make(
            ods::llvm::atomicrmw(
                $builder.context(),
                builder.i16_ty(),
                effective_address,
                value,
                StringAttribute::new(builder.context(), AtomicBinOp::$op.attr_str().as_str())
                    .into(),
                StringAttribute::new(
                    $builder.context(),
                    AtomicOrdering::SequentiallyConsistent.attr_str().as_str(),
                )
                .into(),
                builder.get_insert_location(),
            )
            .into(),
        )?;
        let result = builder.make(arith::extui(
            value,
            builder.i64_ty(),
            builder.get_insert_location(),
        ))?;
        $state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f64());
        return Ok(block);
    };
    ($builder:ident, $state:ident, $memarg:ident, $fcx:ident, $backend:ident, $region:ident, $block:ident, $op:ident, i64, i32) => {
        let (offset, value) = $state.pop2()?;
        let memory_index = MemoryIndex::from_u32(0);
        let (block, effective_address) = Self::resolve_memory_ptr(
            memory_index,
            $memarg,
            offset,
            size_of::<i32>(),
            $fcx,
            $backend.ctx,
            $region,
            $block,
        )?;
        let builder = OpBuilder::new_with_block($builder.context(), block);
        let value = builder.make(arith::trunci(
            value,
            builder.i32_ty(),
            builder.get_insert_location(),
        ))?;
        let result = $builder.make(
            ods::llvm::atomicrmw(
                builder.context(),
                builder.i32_ty(),
                effective_address,
                value,
                StringAttribute::new(builder.context(), AtomicBinOp::$op.attr_str().as_str())
                    .into(),
                StringAttribute::new(
                    builder.context(),
                    AtomicOrdering::SequentiallyConsistent.attr_str().as_str(),
                )
                .into(),
                builder.get_insert_location(),
            )
            .into(),
        )?;
        let result = builder.make(arith::extui(
            value,
            builder.i64_ty(),
            builder.get_insert_location(),
        ))?;
        $state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f64());
        return Ok(block);
    };
}
macro_rules! atomicrmw_cmpxchg_op {
    ($builder:ident, $state:ident, $memarg:ident, $fcx:ident, $backend:ident, $region:ident, $block:ident, $ty:ident, $ty_func:ident) => {
        let (cmp, new) = $state.pop2()?;
        let offset = $state.pop1()?;
        let memory_index = MemoryIndex::from_u32(0);
        let (block, effective_address) = Self::resolve_memory_ptr(
            memory_index,
            $memarg,
            offset,
            size_of::<$ty>(),
            $fcx,
            $backend.ctx,
            $region,
            $block,
        )?;
        let builder = OpBuilder::new_with_block($builder.context(), block);
        let ordering = StringAttribute::new(
            builder.context(),
            AtomicOrdering::SequentiallyConsistent.attr_str().as_str(),
        )
        .into();
        let result = builder.make(
            ods::llvm::cmpxchg(
                builder.context(),
                builder.$ty_func(),
                effective_address,
                cmp,
                new,
                ordering,
                ordering,
                builder.get_insert_location(),
            )
            .into(),
        )?;
        let result = builder.make(llvm::extract_value(
            builder.context(),
            result,
            DenseI64ArrayAttribute::new(builder.context(), &[0]),
            builder.$ty_func(),
            builder.get_insert_location(),
        ))?;
        $state.push1(result.to_ctx_value());
        return Ok(block);
    };
    ($builder:ident, $state:ident, $memarg:ident, $fcx:ident, $backend:ident, $region:ident, $block:ident, $ty:ident, $ty_func:ident, $narrow_ty_func:ident, $extra_ty_func:ident) => {
        let (cmp, new) = $state.pop2()?;
        let offset = $state.pop1()?;
        let memory_index = MemoryIndex::from_u32(0);
        let (block, effective_address) = Self::resolve_memory_ptr(
            memory_index,
            $memarg,
            offset,
            size_of::<$ty>(),
            $fcx,
            $backend.ctx,
            $region,
            $block,
        )?;
        let builder = OpBuilder::new_with_block($builder.context(), block);
        let ordering = StringAttribute::new(
            $builder.context(),
            AtomicOrdering::SequentiallyConsistent.attr_str().as_str(),
        )
        .into();
        let cmp = builder.make(arith::trunci(
            cmp,
            builder.$narrow_ty_func(),
            builder.get_insert_location(),
        ))?;
        let new = builder.make(arith::trunci(
            new,
            builder.$narrow_ty_func(),
            builder.get_insert_location(),
        ))?;
        let result = builder.make(
            ods::llvm::cmpxchg(
                builder.context(),
                builder.$ty_func(),
                effective_address,
                cmp,
                new,
                ordering,
                ordering,
                $builder.get_insert_location(),
            )
            .into(),
        )?;
        let result = builder.make(llvm::extract_value(
            builder.context(),
            result,
            DenseI64ArrayAttribute::new($builder.context(), &[0]),
            builder.$narrow_ty_func(),
            builder.get_insert_location(),
        ))?;
        let result = builder.make(arith::extui(
            result,
            builder.$ty_func(),
            builder.get_insert_location(),
        ))?;
        $state.push1_extra(result.to_ctx_value(), ExtraInfo::$extra_ty_func());
        return Ok(block);
    };
}

/// Contains the execution context required for generating WebAssembly function code. The
/// `FunctionCodeCtx` struct is responsible for storing function-specific information such as local
/// variables, context references, module details, and configuration necessary for code generation.
///
/// # Fields:
/// - `locals`: A vector of tuples, where each tuple consists of a `Type` and a `Value`. This represents
///   the function's local variables and parameters, helping manage data during execution.
/// - `ctx`: The `CtxType` structure, which provides the execution context with cached references
///   to memories, tables, globals, and function offsets required for accessing WebAssembly components.
/// - `unreachable_depth`: A count representing how deep the code is inside unreachable blocks. This is
///   used to manage control flow, especially for branches that cannot be reached during execution.
/// - `memory_styles`: A reference to a `PrimaryMap` of `MemoryIndex` to `MemoryStyle`, representing
///   how memories are organized in the module and controlling properties like memory access strategies.
/// - `_table_styles`: A reference to a `PrimaryMap` of `TableIndex` to `TableStyle`, representing the
///   table types and their styles in the WebAssembly module.
/// - `module`: A reference to the MLIR (Multi-Level Intermediate Representation) `MLIRModule` that
///   represents the structure and instructions of the current WebAssembly module.
/// - `module_translation`: A reference to the `ModuleTranslationState`, which contains the state and
///   context needed for translating the WebAssembly module's components, such as types and signatures.
/// - `wasm_module`: A reference to the `ModuleInfo`, which contains the metadata of the WebAssembly module
///   including details about imports, exports, and module-level properties.
/// - `symbol_registry`: A reference to the `SymbolRegistry` that resolves symbols within the module
///   and aids in linking external components or function calls.
/// - `config`: A reference to the `Config`, containing configuration options and flags that control
///   the code generation process.
///
/// # Example Usage:
/// ```no_check
/// let function_code_ctx = FunctionCodeCtx {
///     locals: vec![(i32_type, value)],
///     ctx: execution_ctx,
///     unreachable_depth: 0,
///     memory_styles: &memory_map,
///     _table_styles: &table_map,
///     module: &mlir_module,
///     module_translation: &module_translation_state,
///     wasm_module: &wasm_module_info,
///     symbol_registry: &symbol_registry_impl,
/// };
/// ```
///
/// # Notes:
/// - The `FunctionCodeCtx` struct is critical for managing the state of code generation during
///   WebAssembly function translation. It encapsulates the necessary elements for maintaining
///   local variables, handling memory styles, and coordinating module translation.
/// - This context is essential for WebAssembly to MLIR-based compilation pipelines, where it
///   ensures proper management of local and global data.
pub struct FunctionCodeCtx<'c, 'a> {
    /// Local variables and function parameters as a vector of type-value pairs.
    pub locals: Vec<(Type<'c>, Value<'c, 'a>)>,
    /// Execution context that provides access to WebAssembly components like memories and globals.
    pub ctx: CtxType<'c, 'a>,
    /// Depth count of unreachable code blocks to manage unreachable code paths.
    pub unreachable_depth: usize,
    /// A reference to memory styles for the WebAssembly module.
    pub memory_styles: &'a PrimaryMap<MemoryIndex, MemoryStyle>,
    /// A reference to table styles for the WebAssembly module.
    pub _table_styles: &'a PrimaryMap<TableIndex, TableStyle>,
    /// State for translating the WebAssembly module.
    pub module_translation: &'a ModuleTranslationState,
    /// Metadata about the WebAssembly module, including imports and exports.
    pub wasm_module: &'a ModuleInfo,
    /// A symbol registry for resolving module symbols.
    pub symbol_registry: &'a dyn SymbolRegistry,
    /// Whether to check static memory bound.
    pub static_memory_bound_check: bool,
}

impl FunctionCodeCtx<'_, '_> {
    #[inline]
    pub(crate) fn is_static_memory_index(&mut self, memory_index: MemoryIndex) -> bool {
        matches!(
            self.ctx.memory(memory_index, self.memory_styles),
            Ok(MemoryCache::Static { base_ptr: _ })
        )
    }
}

pub struct FunctionCodeGenerator;

impl FunctionCodeGenerator {
    /// Translates a WebAssembly function operation into a MLIR module.
    ///
    /// This method handles various WebAssembly operators, translating them into corresponding
    /// MLIR operations within a specified region. It manages the control flow and handles
    /// unreachable code scenarios by keeping track of unreachable depth.
    ///
    /// # Parameters
    /// - `op`: The WebAssembly operator to be translated.
    /// - `fcx`: A mutable reference to the function context containing state and translation data.
    /// - `backend`: A mutable reference to the WASM backend containing context and state information.
    /// - `region`: A reference to the MLIR region where the translated operations will be appended.
    /// - `_source_loc`: A source location identifier (not used in the translation).
    ///
    /// # Returns
    /// A result containing a tuple of `BlockRef<'c, 'a>`, which represents the start and end blocks
    /// of the translated operation.
    ///
    /// # Errors
    /// This function may return an error if the translation of specific operations fails.
    pub fn translate_op<'c, 'a>(
        op: Operator,
        fcx: &mut FunctionCodeCtx<'c, 'a>,
        backend: &mut WASMBackend<'c>,
        region: &'c Region<'c>,
        block: BlockRef<'c, 'a>,
        gas_counter_ptr: Option<Value<'c, 'a>>,
        _source_loc: u32,
    ) -> Result<BlockRef<'c, 'a>>
    where
        'a: 'c,
    {
        let builder = OpBuilder::new_with_block(&backend.ctx.mlir_context, block);
        let state = &mut backend.state;
        let vm_ctx = fcx.ctx.vm_ctx;
        if !state.reachable {
            match op {
                Operator::Block { blockty: _ }
                | Operator::Loop { blockty: _ }
                | Operator::If { blockty: _ } => {
                    fcx.unreachable_depth += 1;
                    return Ok(block);
                }
                Operator::Else => {
                    if fcx.unreachable_depth != 0 {
                        return Ok(block);
                    }
                }
                Operator::End => {
                    if fcx.unreachable_depth != 0 {
                        fcx.unreachable_depth -= 1;
                        return Ok(block);
                    }
                }
                _ => {
                    return Ok(block);
                }
            }
        } else {
            // Insert the gas metering block
            if let Some(gas_counter_ptr) = gas_counter_ptr {
                let location = builder.get_insert_location();
                let gas_value = builder.make(builder.iconst_64(op_gas_cost(&op) as i64))?;
                let gas_counter = builder.make(builder.load(gas_counter_ptr, builder.i64_ty()))?;
                let flag = builder.make(arith::cmpi(
                    builder.context(),
                    arith::CmpiPredicate::Ult,
                    gas_counter,
                    gas_value,
                    location,
                ))?;
                // Out of gas check.
                builder.create(scf::r#if(
                    flag,
                    &[],
                    {
                        let region = Region::new();
                        let block = region.append_block(Block::new(&[]));
                        let builder = OpBuilder::new_with_block(&backend.ctx.mlir_context, block);
                        trap_call(&builder, TrapCode::OutOfGas)?;
                        builder.create(scf::r#yield(&[], location));
                        region
                    },
                    {
                        let region = Region::new();
                        let block = region.append_block(Block::new(&[]));
                        let builder = OpBuilder::new_with_block(&backend.ctx.mlir_context, block);
                        let new_gas_counter =
                            builder.make(arith::subi(gas_counter, gas_value, location))?;
                        builder.create(builder.store(new_gas_counter, gas_counter_ptr));
                        builder.create(scf::r#yield(&[], location));
                        region
                    },
                    location,
                ));
            }
        }
        match op {
            /***************************
             * Control Flow instructions.
             * https://github.com/sunfishcode/wasm-reference-manual/blob/master/WebAssembly.md#control-flow-instructions
             ***************************/
            Operator::Unreachable => {
                trap_call(&builder, TrapCode::UnreachableCodeReached)?;
                builder.create(
                    dora_ir::wasm::unreachable(builder.context(), builder.unknown_loc()).into(),
                );
                state.reachable = false;
            }
            Operator::Nop => {
                // Do nothing.
            }
            Operator::Block { blockty } => {
                let phis: SmallVec<[(Type<'c>, Location); 1]> = fcx
                    .module_translation
                    .blocktype_params_results(&blockty)?
                    .1
                    .iter()
                    .map(|&wp_ty| {
                        wptype_to_type(wp_ty).map(|wasm_ty| {
                            (
                                type_to_mlir(&backend.intrinsics, &wasm_ty),
                                builder.unknown_loc(),
                            )
                        })
                    })
                    .collect::<WasmResult<_>>()?;
                let end_block = region.append_block(Block::new(&phis));
                state.push_block(end_block, phis);
            }
            Operator::Loop { blockty } => {
                let blocktypes = fcx.module_translation.blocktype_params_results(&blockty)?;
                let phis: SmallVec<[PhiValue<'c>; 1]> = blocktypes
                    .1
                    .iter()
                    .map(|&wp_ty| {
                        wptype_to_type(wp_ty).map(|wasm_ty| {
                            (
                                type_to_mlir(&backend.intrinsics, &wasm_ty),
                                builder.unknown_loc(),
                            )
                        })
                    })
                    .collect::<WasmResult<_>>()?;
                let loop_next = region.append_block(Block::new(&phis));
                let loop_phis: SmallVec<[PhiValue<'c>; 1]> = blocktypes
                    .0
                    .iter()
                    .map(|&wp_ty| {
                        wptype_to_type(wp_ty).map(|wasm_ty| {
                            (
                                type_to_mlir(&backend.intrinsics, &wasm_ty),
                                builder.unknown_loc(),
                            )
                        })
                    })
                    .collect::<WasmResult<_>>()?;
                let loop_body = region.append_block(Block::new(&loop_phis));
                let values = state.peekn(loop_phis.len())?;
                state.popn(loop_phis.len())?;
                builder.create(cf::br(&loop_body, &values, builder.unknown_loc()));
                state.push_loop(loop_body, loop_next, loop_phis, phis);
                for i in 0..loop_body.argument_count() {
                    let value: Value = loop_body.argument(i)?.into();
                    state.push1(value.to_ctx_value());
                }
                return Ok(loop_body);
            }
            Operator::If { blockty } => {
                let block_param_types = fcx
                    .module_translation
                    .blocktype_params_results(&blockty)?
                    .0
                    .iter()
                    .map(|&wp_ty| {
                        wptype_to_type(wp_ty).map(|wasm_ty| {
                            (
                                type_to_mlir(&backend.intrinsics, &wasm_ty),
                                builder.unknown_loc(),
                            )
                        })
                    })
                    .collect::<WasmResult<Vec<_>>>()?;

                let then_phis: SmallVec<[PhiValue<'c>; 1]> =
                    block_param_types.iter().copied().collect();
                let else_phis: SmallVec<[PhiValue<'c>; 1]> =
                    block_param_types.iter().copied().collect();

                let end_phis: SmallVec<[PhiValue<'c>; 1]> = fcx
                    .module_translation
                    .blocktype_params_results(&blockty)?
                    .1
                    .iter()
                    .map(|&wp_ty| {
                        wptype_to_type(wp_ty).map(|wasm_ty| {
                            (
                                type_to_mlir(&backend.intrinsics, &wasm_ty),
                                builder.unknown_loc(),
                            )
                        })
                    })
                    .collect::<WasmResult<_>>()?;

                let if_then_block = region.append_block(Block::new(&then_phis));
                // Note that the else block here may be an empty block that can be ignored.
                let if_else_block = region.append_block(Block::new(&else_phis));
                let end_block = region.append_block(Block::new(&end_phis));
                let cond = state.pop1()?;
                let cond = builder.make(builder.icmp_imm(IntCC::NotEqual, cond, 0)?)?;
                let values = state.peekn(then_phis.len())?;
                state.popn(then_phis.len())?;
                builder.create(cf::cond_br(
                    builder.context(),
                    cond,
                    &if_then_block,
                    &if_else_block,
                    &values,
                    &values,
                    builder.unknown_loc(),
                ));
                for i in 0..if_then_block.argument_count() {
                    let value: Value = if_then_block.argument(i)?.into();
                    state.push1(value.to_ctx_value());
                }
                state.push_if(
                    if_then_block,
                    if_else_block,
                    end_block,
                    then_phis,
                    else_phis,
                    end_phis,
                );
                return Ok(if_then_block);
            }
            Operator::Else => {
                if state.reachable {
                    let frame = state.frame_at_depth(0)?;
                    let values = state.peekn(frame.phis().len())?;
                    state.popn(frame.phis().len())?;
                    let frame = state.frame_at_depth(0)?;
                    builder.create(cf::br(
                        frame.code_after(),
                        &values,
                        builder.get_insert_location(),
                    ));
                }

                {
                    let (_, if_else_state) = if let ControlFrame::IfElse {
                        if_else,
                        if_else_state,
                        ..
                    } = state.frame_at_depth_mut(0)?
                    {
                        (if_else, if_else_state)
                    } else {
                        unreachable!()
                    };
                    *if_else_state = IfElseState::Else;
                }
                state.reachable = true;

                let if_else_block =
                    if let ControlFrame::IfElse { if_else, .. } = state.frame_at_depth(0)? {
                        *if_else
                    } else {
                        unreachable!()
                    };
                for i in 0..if_else_block.argument_count() {
                    let value: Value = if_else_block.argument(i)?.into();
                    state.push1(value.to_ctx_value());
                }
                return Ok(if_else_block);
            }
            Operator::End => {
                let frame = state.pop_frame()?;
                if state.reachable {
                    let values = state.peekn(frame.phis().len())?;
                    state.popn(frame.phis().len())?;
                    builder.create(cf::br(
                        frame.code_after(),
                        &values,
                        builder.get_insert_location(),
                    ));
                }

                // No else branch
                if let ControlFrame::IfElse {
                    if_else,
                    next,
                    if_else_state: IfElseState::If,
                    ..
                } = &frame
                {
                    let mut values = vec![];
                    for i in 0..if_else.argument_count() {
                        let value: Value = if_else.argument(i)?.into();
                        values.push(value.to_ctx_value());
                    }
                    (*if_else).append_operation(cf::br(
                        next,
                        &values,
                        builder.get_insert_location(),
                    ));
                }
                state.reset_stack(&frame);
                state.reachable = true;
                for i in 0..frame.code_after().argument_count() {
                    let value: Value = frame.code_after().argument(i)?.into();
                    state.push1(value.to_ctx_value());
                }

                return Ok(*frame.code_after());
            }
            Operator::Br { relative_depth } => {
                let frame = state.frame_at_depth(relative_depth)?;

                let phis = if frame.is_loop() {
                    frame.loop_body_phis()
                } else {
                    frame.phis()
                };

                let len = phis.len();
                let values = state.peekn(len)?;

                // For each result of the block we're branching to,
                // pop a value off the value stack and load it into
                // the corresponding phi.
                builder.create(cf::br(
                    frame.br_dest(),
                    &values,
                    builder.get_insert_location(),
                ));

                state.popn(len)?;
                state.reachable = false;
            }
            Operator::BrIf { relative_depth } => {
                let cond = state.pop1()?;
                let frame = state.frame_at_depth(relative_depth)?;
                let phis = if frame.is_loop() {
                    frame.loop_body_phis()
                } else {
                    frame.phis()
                };
                let param_stack = state.peekn(phis.len())?;
                let else_block = region.append_block(Block::new(phis));
                let cond_value = builder.create(builder.icmp_imm(IntCC::NotEqual, cond, 0)?);
                builder.create(cf::cond_br(
                    builder.context(),
                    cond_value.result(0)?.into(),
                    frame.br_dest(),
                    &else_block,
                    &param_stack,
                    &param_stack,
                    builder.get_insert_location(),
                ));
                return Ok(else_block);
            }
            Operator::BrTable { targets } => {
                let index = state.pop1()?;
                let default_frame = state.frame_at_depth(targets.default())?;
                let phis = if default_frame.is_loop() {
                    default_frame.loop_body_phis()
                } else {
                    default_frame.phis()
                };
                let args = state.peekn(phis.len())?;

                let mut case_values = Vec::new();
                let mut case_destinations = Vec::new();
                for (case_index, depth) in targets.targets().enumerate() {
                    let depth = depth.map_err(from_binaryreadererror_wasmerror)?;
                    let frame = state.frame_at_depth(depth)?;
                    case_values.push(case_index as i64);
                    case_destinations.push(*frame.br_dest());
                }
                let case_destinations: Vec<_> = case_destinations
                    .iter()
                    .map(|b| {
                        let x: (&Block, &[Value]) = (b, &args);
                        x
                    })
                    .collect();

                builder.create(cf::switch(
                    builder.context(),
                    &case_values,
                    index,
                    builder.i32_ty(),
                    (default_frame.br_dest(), &args),
                    &case_destinations,
                    builder.get_insert_location(),
                )?);

                let args_len = args.len();
                state.popn(args_len)?;
                state.reachable = false;
            }
            Operator::Return => {
                let frame = state.outermost_frame()?;
                let values = state.peekn(frame.phis().len())?;
                builder.create(cf::br(
                    frame.br_dest(),
                    &values,
                    builder.get_insert_location(),
                ));
                state.reachable = false;
            }
            Operator::Call { function_index } => {
                let func_index = FunctionIndex::from_u32(function_index);
                let sigindex = fcx.wasm_module.functions[func_index];
                let wasm_func_type = &fcx.wasm_module.signatures[sigindex];
                let _import_key = fcx.ctx.get_import_function_info(function_index);
                let (
                    FunctionCache {
                        func_type,
                        func_with_vmctx,
                    },
                    func_name,
                ) = if let Some(local_func_index) = fcx.wasm_module.local_func_index(func_index) {
                    let (func_name, _) = FuncTranslator::get_func_name(
                        fcx.wasm_module,
                        &local_func_index,
                        &func_index,
                        fcx.symbol_registry,
                    );
                    (
                        fcx.ctx.local_func(
                            local_func_index,
                            func_index,
                            wasm_func_type,
                            backend.ctx,
                            &backend.intrinsics,
                        )?,
                        func_name,
                    )
                } else {
                    (
                        fcx.ctx.func(
                            func_index,
                            wasm_func_type,
                            backend.ctx,
                            &backend.intrinsics,
                        )?,
                        "".to_string(),
                    )
                };
                let args = state.popn_save_extra(wasm_func_type.params().len())?;
                let args = args.iter().map(|p| p.0).collect::<Vec<Value<'_, '_>>>();
                let result_count = func_type.result_count();
                let return_types = (0..result_count)
                    .map(|i| func_type.result(i).unwrap())
                    .collect::<Vec<_>>();
                // Call import host functions
                let op = if let Some((func_ptr, vm_ctx)) = func_with_vmctx {
                    debug_assert!(return_types.len() <= 1);
                    let args = std::iter::once(*vm_ctx)
                        .chain(args.iter().copied())
                        .collect::<Vec<Value<'_, '_>>>();
                    let ret_ty = if !return_types.is_empty() {
                        return_types[0]
                    } else {
                        builder.intrinsics.void_ty
                    };
                    builder.create(builder.indirect_call(ret_ty, *func_ptr, &args)?)
                } else {
                    // Call local functions
                    let args = std::iter::once(vm_ctx)
                        .chain(args.iter().copied())
                        .collect::<Vec<Value<'_, '_>>>();
                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(&backend.ctx.mlir_context, &func_name),
                        &args,
                        &return_types,
                        builder.get_insert_location(),
                    ))
                };
                for i in 0..result_count {
                    let value = op.result(i)?.to_ctx_value();
                    state.push1(value);
                }
            }
            Operator::Drop => {
                state.pop1()?;
            }
            // `TypedSelect` must be used for extern refs so ref counting should
            // be done with TypedSelect. But otherwise they're the same.
            Operator::TypedSelect { .. } | Operator::Select => {
                let ((v1, i1), (v2, i2), (cond, _)) = state.pop3_extra()?;
                // If the pending bits of v1 and v2 are the same, we can pass
                // them along to the result. Otherwise, apply pending
                // canonicalizations now.
                let (v1, i1, v2, i2) = if i1.has_pending_f32_nan() != i2.has_pending_f32_nan()
                    || i1.has_pending_f64_nan() != i2.has_pending_f64_nan()
                {
                    (v1, i1.strip_pending(), v2, i2.strip_pending())
                } else {
                    (v1, i1, v2, i2)
                };
                let res = builder.make(
                    dora_ir::wasm::select(
                        builder.context(),
                        v1.r#type(),
                        v1,
                        v2,
                        cond,
                        builder.get_insert_location(),
                    )
                    .into(),
                )?;
                let info = {
                    let mut info = i1.strip_pending() & i2.strip_pending();
                    if i1.has_pending_f32_nan() {
                        debug_assert!(i2.has_pending_f32_nan());
                        info |= ExtraInfo::pending_f32_nan();
                    }
                    if i1.has_pending_f64_nan() {
                        debug_assert!(i2.has_pending_f64_nan());
                        info |= ExtraInfo::pending_f64_nan();
                    }
                    info
                };
                state.push1_extra(res.to_ctx_value(), info);
            }
            Operator::LocalGet { local_index } => {
                let (type_value, pointer_value) = fcx.locals[local_index as usize];
                let v = builder.make(builder.load(pointer_value.to_ctx_value(), type_value))?;
                state.push1(v.to_ctx_value());
            }
            Operator::LocalSet { local_index } => {
                let pointer_value = fcx.locals[local_index as usize].1;
                let v = state.pop1()?;
                builder.create(builder.store(v, pointer_value));
            }
            Operator::LocalTee { local_index } => {
                let pointer_value = fcx.locals[local_index as usize].1;
                let v = state.peek1()?;
                builder.create(builder.store(v, pointer_value));
            }
            Operator::GlobalGet { global_index } => {
                let global_index = GlobalIndex::from_u32(global_index);
                match fcx.ctx.global(global_index, &backend.intrinsics)? {
                    GlobalCache::Const { value } => {
                        state.push1(value.to_ctx_value());
                    }
                    GlobalCache::Mut {
                        ptr_to_value,
                        value_type,
                    } => {
                        let builder = OpBuilder::new_with_block(builder.context(), block);
                        let value = builder.make(builder.load(ptr_to_value, value_type))?;
                        state.push1(value.to_ctx_value());
                    }
                }
            }
            Operator::GlobalSet { global_index } => {
                let global_index = GlobalIndex::from_u32(global_index);
                match fcx.ctx.global(global_index, &backend.intrinsics)? {
                    GlobalCache::Const { value: _ } => {
                        return Err(CompileError::Codegen(format!(
                            "global.set on immutable global index {}",
                            global_index.as_u32()
                        ))
                        .into());
                    }
                    GlobalCache::Mut { ptr_to_value, .. } => {
                        let value = state.pop1()?;
                        builder.create(builder.store(value, ptr_to_value));
                    }
                }
            }
            Operator::I32Load { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    4,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.i32_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                state.push1(result.to_ctx_value());
                return Ok(block);
            }
            Operator::I64Load { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    8,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.i64_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                state.push1(result.to_ctx_value());
                return Ok(block);
            }
            Operator::F32Load { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    4,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.f32_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                state.push1(result.to_ctx_value());
                return Ok(block);
            }
            Operator::F64Load { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    8,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.f64_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                state.push1(result.to_ctx_value());
                return Ok(block);
            }
            Operator::I32Load8S { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    1,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.i8_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                let result = builder.make(arith::extsi(
                    result,
                    builder.i32_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1(result.to_ctx_value());
                return Ok(block);
            }
            Operator::I32Load8U { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    1,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.i8_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                let result = builder.make(arith::extui(
                    result,
                    builder.i32_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f32());
                return Ok(block);
            }
            Operator::I32Load16S { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    2,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.i16_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                let result = builder.make(arith::extsi(
                    result,
                    builder.i32_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1(result.to_ctx_value());
                return Ok(block);
            }
            Operator::I32Load16U { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    2,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.i16_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                let result = builder.make(arith::extui(
                    result,
                    builder.i32_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f32());
                return Ok(block);
            }
            Operator::I64Load8S { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    1,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.i8_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                let result = builder.make(arith::extsi(
                    result,
                    builder.i64_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1(result.to_ctx_value());
                return Ok(block);
            }
            Operator::I64Load8U { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    1,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.i8_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                let result = builder.make(arith::extui(
                    result,
                    builder.i64_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f64());
                return Ok(block);
            }
            Operator::I64Load16S { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    2,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.i16_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                let result = builder.make(arith::extsi(
                    result,
                    builder.i64_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1(result.to_ctx_value());
                return Ok(block);
            }
            Operator::I64Load16U { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    2,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.i16_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                let result = builder.make(arith::extui(
                    result,
                    builder.i64_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f32());
                return Ok(block);
            }
            Operator::I64Load32S { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    4,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.i32_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                let result = builder.make(arith::extsi(
                    result,
                    builder.i64_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1(result.to_ctx_value());
                return Ok(block);
            }
            Operator::I64Load32U { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    4,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_align_and_volatile(
                    effective_address,
                    builder.i32_ty(),
                    1,
                    fcx.is_static_memory_index(memory_index),
                ))?;
                let result = builder.make(arith::extui(
                    result,
                    builder.i64_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f32());
                return Ok(block);
            }
            Operator::I32Store { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    4,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                builder.create(builder.store_with_align_and_volatile(
                    value,
                    effective_address,
                    1,
                    fcx.is_static_memory_index(memory_index),
                ));
                return Ok(block);
            }
            Operator::I64Store { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    8,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                builder.create(builder.store_with_align_and_volatile(
                    value,
                    effective_address,
                    1,
                    fcx.is_static_memory_index(memory_index),
                ));
                return Ok(block);
            }
            Operator::F32Store { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    4,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                builder.create(builder.store_with_align_and_volatile(
                    value,
                    effective_address,
                    1,
                    fcx.is_static_memory_index(memory_index),
                ));
                return Ok(block);
            }
            Operator::F64Store { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    8,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                builder.create(builder.store_with_align_and_volatile(
                    value,
                    effective_address,
                    1,
                    fcx.is_static_memory_index(memory_index),
                ));
                return Ok(block);
            }
            Operator::I32Store8 { ref memarg } | Operator::I64Store8 { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    1,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let value = builder.make(arith::trunci(
                    value,
                    builder.i8_ty(),
                    builder.get_insert_location(),
                ))?;
                builder.create(builder.store_with_align_and_volatile(
                    value,
                    effective_address,
                    1,
                    fcx.is_static_memory_index(memory_index),
                ));
                return Ok(block);
            }
            Operator::I32Store16 { ref memarg } | Operator::I64Store16 { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    2,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let value = builder.make(arith::trunci(
                    value,
                    builder.i16_ty(),
                    builder.get_insert_location(),
                ))?;
                builder.create(builder.store_with_align_and_volatile(
                    value,
                    effective_address,
                    1,
                    fcx.is_static_memory_index(memory_index),
                ));
                return Ok(block);
            }
            Operator::I64Store32 { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    4,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let value = builder.make(arith::trunci(
                    value,
                    builder.i32_ty(),
                    builder.get_insert_location(),
                ))?;
                builder.create(builder.store_with_align_and_volatile(
                    value,
                    effective_address,
                    1,
                    fcx.is_static_memory_index(memory_index),
                ));
                return Ok(block);
            }
            Operator::I32Const { value } => {
                let i = builder
                    .create(builder.iconst_32(value))
                    .result(0)?
                    .to_ctx_value();
                let info = if is_f32_arithmetic(value as u32) {
                    ExtraInfo::arithmetic_f32()
                } else {
                    Default::default()
                };
                state.push1_extra(i, info);
            }
            Operator::I64Const { value } => {
                let i = builder
                    .create(builder.iconst_64(value))
                    .result(0)?
                    .to_ctx_value();
                let info = if is_f64_arithmetic(value as u64) {
                    ExtraInfo::arithmetic_f64()
                } else {
                    Default::default()
                };
                state.push1_extra(i, info);
            }
            Operator::F32Const { value } => {
                let bits = value.bits();
                let info = if is_f32_arithmetic(bits) {
                    ExtraInfo::arithmetic_f32()
                } else {
                    Default::default()
                };
                let bits = builder.make(builder.iconst_32(bits as i32))?;
                let f = builder.make(arith::bitcast(
                    bits,
                    builder.f32_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1_extra(f.to_ctx_value(), info);
            }
            Operator::F64Const { value } => {
                let bits = value.bits();
                let info = if is_f64_arithmetic(bits) {
                    ExtraInfo::arithmetic_f64()
                } else {
                    Default::default()
                };
                let bits = builder.make(builder.iconst_64(bits as i64))?;
                let f = builder.make(arith::bitcast(
                    bits,
                    builder.f64_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1_extra(f.to_ctx_value(), info);
            }
            /***************************
             * Reference types.
             * https://github.com/WebAssembly/reference-types/blob/master/proposals/reference-types/Overview.md
             ***************************/
            Operator::RefNull { hty } => {
                let ty = wpheaptype_to_type(hty)?;

                let attr = type_to_mlir_zero_attribute(builder.context(), &backend.intrinsics, ty);
                let value = if matches!(ty, wasmer::Type::FuncRef | wasmer::Type::ExternRef) {
                    builder
                        .make(llvm::zero(builder.ptr_ty(), builder.get_insert_location()))?
                        .to_ctx_value()
                } else {
                    builder
                        .make(arith::constant(
                            builder.context(),
                            attr,
                            builder.unknown_loc(),
                        ))?
                        .to_ctx_value()
                };
                state.push1(value);
            }
            Operator::RefIsNull => {
                // Note value is a pointer type
                let value = state.pop1()?;
                let result = is_zero(&builder, value)?;
                let result_i32 = builder.make(arith::extui(
                    result,
                    builder.i32_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1(result_i32.to_ctx_value())
            }
            Operator::RefFunc { function_index } => {
                let index = builder.make(builder.iconst_32(function_index as i32))?;
                let value = builder
                    .make(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::FUNC_REF,
                        ),
                        &[vm_ctx, index],
                        &[builder.ptr_ty()],
                        builder.get_insert_location(),
                    ))?
                    .to_ctx_value();
                state.push1(value);
            }
            Operator::I32Eqz => {
                op!(builder, state, eqz, i32);
            }
            Operator::I32Eq => {
                bin_op!(builder, state, eq, i32);
            }
            Operator::I32Ne => {
                bin_op!(builder, state, ne, i32);
            }
            Operator::I32LtS => {
                bin_op!(builder, state, lts, i32);
            }
            Operator::I32LtU => {
                bin_op!(builder, state, ltu, i32);
            }
            Operator::I32GtS => {
                bin_op!(builder, state, gts, i32);
            }
            Operator::I32GtU => {
                bin_op!(builder, state, gtu, i32);
            }
            Operator::I32LeS => {
                bin_op!(builder, state, les, i32);
            }
            Operator::I32LeU => {
                bin_op!(builder, state, leu, i32);
            }
            Operator::I32GeS => {
                bin_op!(builder, state, ges, i32);
            }
            Operator::I32GeU => {
                bin_op!(builder, state, geu, i32);
            }
            Operator::I64Eqz => {
                op!(builder, state, eqz, i64 => i32);
            }
            Operator::I64Eq => {
                bin_op!(builder, state, eq, i64 => i32);
            }
            Operator::I64Ne => {
                bin_op!(builder, state, ne, i64 => i32);
            }
            Operator::I64LtS => {
                bin_op!(builder, state, lts, i64 => i32);
            }
            Operator::I64LtU => {
                bin_op!(builder, state, ltu, i64 => i32);
            }
            Operator::I64GtS => {
                bin_op!(builder, state, gts, i64 => i32);
            }
            Operator::I64GtU => {
                bin_op!(builder, state, gtu, i64 => i32);
            }
            Operator::I64LeS => {
                bin_op!(builder, state, les, i64 => i32);
            }
            Operator::I64LeU => {
                bin_op!(builder, state, leu, i64 => i32);
            }
            Operator::I64GeS => {
                bin_op!(builder, state, ges, i64 => i32);
            }
            Operator::I64GeU => {
                bin_op!(builder, state, geu, i64 => i32);
            }
            Operator::F32Eq => {
                bin_op!(builder, state, eq, f32 => i32);
            }
            Operator::F32Ne => {
                bin_op!(builder, state, ne, f32 => i32);
            }
            Operator::F32Lt => {
                bin_op!(builder, state, flt, f32 => i32);
            }
            Operator::F32Gt => {
                bin_op!(builder, state, fgt, f32 => i32);
            }
            Operator::F32Le => {
                bin_op!(builder, state, fle, f32 => i32);
            }
            Operator::F32Ge => {
                bin_op!(builder, state, fge, f32 => i32);
            }
            Operator::F64Eq => {
                bin_op!(builder, state, eq, f64 => i32);
            }
            Operator::F64Ne => {
                bin_op!(builder, state, ne, f64 => i32);
            }
            Operator::F64Lt => {
                bin_op!(builder, state, flt, f64 => i32);
            }
            Operator::F64Gt => {
                bin_op!(builder, state, fgt, f64 => i32);
            }
            Operator::F64Le => {
                bin_op!(builder, state, fle, f64 => i32);
            }
            Operator::F64Ge => {
                bin_op!(builder, state, fge, f64 => i32);
            }
            Operator::I32Clz => {
                op!(builder, state, clz, i32);
            }
            Operator::I32Ctz => {
                op!(builder, state, ctz, i32);
            }
            Operator::I32Popcnt => {
                op!(builder, state, popcnt, i32);
            }
            Operator::I32Add => {
                bin_op!(builder, state, add, i32);
            }
            Operator::I32Sub => {
                bin_op!(builder, state, sub, i32);
            }
            Operator::I32Mul => {
                bin_op!(builder, state, mul, i32);
            }
            Operator::I32DivS => {
                bin_op!(builder, state, divs, i32);
            }
            Operator::I32DivU => {
                bin_op!(builder, state, divu, i32);
            }
            Operator::I32RemS => {
                bin_op!(builder, state, rems, i32);
            }
            Operator::I32RemU => {
                bin_op!(builder, state, remu, i32);
            }
            Operator::I32And => {
                bin_op!(builder, state, and, i32);
            }
            Operator::I32Or => {
                bin_op!(builder, state, or, i32);
            }
            Operator::I32Xor => {
                bin_op!(builder, state, xor, i32);
            }
            Operator::I32Shl => {
                bin_op!(builder, state, shl, i32);
            }
            Operator::I32ShrS => {
                bin_op!(builder, state, shrs, i32);
            }
            Operator::I32ShrU => {
                bin_op!(builder, state, shru, i32);
            }
            Operator::I32Rotl => {
                bin_op!(builder, state, rotl, i32);
            }
            Operator::I32Rotr => {
                bin_op!(builder, state, rotr, i32);
            }
            Operator::I64Clz => {
                op!(builder, state, clz, i64);
            }
            Operator::I64Ctz => {
                op!(builder, state, ctz, i64);
            }
            Operator::I64Popcnt => {
                op!(builder, state, popcnt, i64);
            }
            Operator::I64Add => {
                bin_op!(builder, state, add, i64);
            }
            Operator::I64Sub => {
                bin_op!(builder, state, sub, i64);
            }
            Operator::I64Mul => {
                bin_op!(builder, state, mul, i64);
            }
            Operator::I64DivS => {
                bin_op!(builder, state, divs, i64);
            }
            Operator::I64DivU => {
                bin_op!(builder, state, divu, i64);
            }
            Operator::I64RemS => {
                bin_op!(builder, state, rems, i64);
            }
            Operator::I64RemU => {
                bin_op!(builder, state, remu, i64);
            }
            Operator::I64And => {
                bin_op!(builder, state, and, i64);
            }
            Operator::I64Or => {
                bin_op!(builder, state, or, i64);
            }
            Operator::I64Xor => {
                bin_op!(builder, state, xor, i64);
            }
            Operator::I64Shl => {
                bin_op!(builder, state, shl, i64);
            }
            Operator::I64ShrS => {
                bin_op!(builder, state, shrs, i64);
            }
            Operator::I64ShrU => {
                bin_op!(builder, state, shru, i64);
            }
            Operator::I64Rotl => {
                bin_op!(builder, state, rotl, i64);
            }
            Operator::I64Rotr => {
                bin_op!(builder, state, rotr, i64);
            }
            Operator::F32Abs => {
                op!(builder, state, abs, f32);
            }
            Operator::F32Neg => {
                op!(builder, state, neg, f32);
            }
            Operator::F32Ceil => {
                op!(builder, state, ceil, f32);
            }
            Operator::F32Floor => {
                op!(builder, state, floor, f32);
            }
            Operator::F32Trunc => {
                op!(builder, state, trunc, f32);
            }
            Operator::F32Nearest => {
                op!(builder, state, nearest, f32);
            }
            Operator::F32Sqrt => {
                op!(builder, state, sqrt, f32);
            }
            Operator::F32Add => {
                bin_op!(builder, state, add, f32);
            }
            Operator::F32Sub => {
                bin_op!(builder, state, sub, f32);
            }
            Operator::F32Mul => {
                bin_op!(builder, state, mul, f32);
            }
            Operator::F32Div => {
                bin_op!(builder, state, divs, f32);
            }
            Operator::F32Min => {
                bin_op!(builder, state, min, f32);
            }
            Operator::F32Max => {
                bin_op!(builder, state, max, f32);
            }
            Operator::F32Copysign => {
                bin_op!(builder, state, copysign, f32);
            }
            Operator::F64Abs => {
                op!(builder, state, abs, f64);
            }
            Operator::F64Neg => {
                op!(builder, state, neg, f64);
            }
            Operator::F64Ceil => {
                op!(builder, state, ceil, f64);
            }
            Operator::F64Floor => {
                op!(builder, state, floor, f64);
            }
            Operator::F64Trunc => {
                op!(builder, state, trunc, f64);
            }
            Operator::F64Nearest => {
                op!(builder, state, nearest, f64);
            }
            Operator::F64Sqrt => {
                op!(builder, state, sqrt, f64);
            }
            Operator::F64Add => {
                bin_op!(builder, state, add, f64);
            }
            Operator::F64Sub => {
                bin_op!(builder, state, sub, f64);
            }
            Operator::F64Mul => {
                bin_op!(builder, state, mul, f64);
            }
            Operator::F64Div => {
                bin_op!(builder, state, divs, f64);
            }
            Operator::F64Min => {
                bin_op!(builder, state, min, f64);
            }
            Operator::F64Max => {
                bin_op!(builder, state, max, f64);
            }
            Operator::F64Copysign => {
                bin_op!(builder, state, copysign, f64);
            }
            /***************************
             * Conversion instructions.
             * https://github.com/sunfishcode/wasm-reference-manual/blob/master/WebAssembly.md#conversion-instructions
             ***************************/
            Operator::I32WrapI64 => {
                op!(builder, state, i_32_wrap_i_64, i32);
            }
            Operator::I32TruncF32S => {
                let block = trap_float_if_not_representable_as_int(
                    backend.ctx,
                    region,
                    block,
                    0xcf000000, // -2147483600.0
                    0x4effffff, // 2147483500.0
                    state.peek1()?,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                op!(builder, state, i_32_trunc_f_32_s, i32);
                return Ok(block);
            }
            Operator::I32TruncF32U => {
                let block = trap_float_if_not_representable_as_int(
                    backend.ctx,
                    region,
                    block,
                    0xbf7fffff, // -0.99999994
                    0x4f7fffff, // 4294967000.0
                    state.peek1()?,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                op!(builder, state, i_32_trunc_f_32_u, i32);
                return Ok(block);
            }
            Operator::I32TruncF64S => {
                let block = trap_float_if_not_representable_as_int(
                    backend.ctx,
                    region,
                    block,
                    0xc1e00000001fffff, // -2147483648.9999995
                    0x41dfffffffffffff, // 2147483647.9999998
                    state.peek1()?,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                op!(builder, state, i_32_trunc_f_64_s, i32);
                return Ok(block);
            }
            Operator::I32TruncF64U => {
                let block = trap_float_if_not_representable_as_int(
                    backend.ctx,
                    region,
                    block,
                    0xbfefffffffffffff, // -0.9999999999999999
                    0x41efffffffffffff, // 4294967295.9999995
                    state.peek1()?,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                op!(builder, state, i_32_trunc_f_64_u, i32);
                return Ok(block);
            }
            Operator::I64ExtendI32S => {
                op!(builder, state, i_64_extend_i_32_s, i64);
            }
            Operator::I64ExtendI32U => {
                op!(builder, state, i_64_extend_i_32_u, i64);
            }
            Operator::I64TruncF32S => {
                let block = trap_float_if_not_representable_as_int(
                    backend.ctx,
                    region,
                    block,
                    0xdf000000, // -9223372000000000000.0
                    0x5effffff, // 9223371500000000000.0
                    state.peek1()?,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                op!(builder, state, i_64_trunc_f_32_s, i64);
                return Ok(block);
            }
            Operator::I64TruncF32U => {
                let block = trap_float_if_not_representable_as_int(
                    backend.ctx,
                    region,
                    block,
                    0xbf7fffff, // -0.99999994
                    0x5f7fffff, // 18446743000000000000.0
                    state.peek1()?,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                op!(builder, state, i_64_trunc_f_32_u, i64);
                return Ok(block);
            }
            Operator::I64TruncF64S => {
                let block = trap_float_if_not_representable_as_int(
                    backend.ctx,
                    region,
                    block,
                    0xc3e0000000000000, // -9223372036854776000.0
                    0x43dfffffffffffff, // 9223372036854775000.0
                    state.peek1()?,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                op!(builder, state, i_64_trunc_f_64_s, i64);
                return Ok(block);
            }
            Operator::I64TruncF64U => {
                let block = trap_float_if_not_representable_as_int(
                    backend.ctx,
                    region,
                    block,
                    0xbfefffffffffffff, // -0.9999999999999999
                    0x43efffffffffffff, // 18446744073709550000.0
                    state.peek1()?,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                op!(builder, state, i_64_trunc_f_64_u, i64);
                return Ok(block);
            }
            Operator::F32ConvertI32S => {
                op!(builder, state, f_32_convert_i_32_s, f32);
            }
            Operator::F32ConvertI32U => {
                op!(builder, state, f_32_convert_i_32_u, f32);
            }
            Operator::F32ConvertI64S => {
                op!(builder, state, f_32_convert_i_64_s, f32);
            }
            Operator::F32ConvertI64U => {
                op!(builder, state, f_32_convert_i_64_u, f32);
            }
            Operator::F32DemoteF64 => {
                op!(builder, state, f_32_demote_f_64, f32);
            }
            Operator::F64ConvertI32S => {
                op!(builder, state, f_64_convert_i_32_s, f64);
            }
            Operator::F64ConvertI32U => {
                op!(builder, state, f_64_convert_i_32_u, f64);
            }
            Operator::F64ConvertI64S => {
                op!(builder, state, f_64_convert_i_64_s, f64);
            }
            Operator::F64ConvertI64U => {
                op!(builder, state, f_64_convert_i_64_u, f64);
            }
            Operator::F64PromoteF32 => {
                op!(builder, state, f_64_promote_f_32, f64);
            }
            Operator::I32ReinterpretF32 => {
                op!(builder, state, i_32_reinterpret_f_32, i32);
            }
            Operator::I64ReinterpretF64 => {
                op!(builder, state, i_64_reinterpret_f_64, i64);
            }
            Operator::F32ReinterpretI32 => {
                op!(builder, state, f_32_reinterpret_i_32, f32);
            }
            Operator::F64ReinterpretI64 => {
                op!(builder, state, f_64_reinterpret_i_64, f64);
            }
            Operator::I32Extend8S => {
                op!(builder, state, i_32_extend_8_s, i32);
            }
            Operator::I32Extend16S => {
                op!(builder, state, i_32_extend_16_s, i32);
            }
            Operator::I64Extend8S => {
                op!(builder, state, i_64_extend_8_s, i64);
            }
            Operator::I64Extend16S => {
                op!(builder, state, i_64_extend_16_s, i64);
            }
            Operator::I64Extend32S => {
                op!(builder, state, i_64_extend_32_s, i64);
            }
            Operator::I32TruncSatF32S => {
                op!(builder, state, i_32_trunc_sat_f_32_s, i32);
            }
            Operator::I32TruncSatF32U => {
                op!(builder, state, i_32_trunc_sat_f_32_u, i32);
            }
            Operator::I32TruncSatF64S => {
                op!(builder, state, i_32_trunc_sat_f_64_s, i32);
            }
            Operator::I32TruncSatF64U => {
                op!(builder, state, i_32_trunc_sat_f_64_u, i32);
            }
            Operator::I64TruncSatF32S => {
                op!(builder, state, i_64_trunc_sat_f_32_s, i64);
            }
            Operator::I64TruncSatF32U => {
                op!(builder, state, i_64_trunc_sat_f_32_u, i64);
            }
            Operator::I64TruncSatF64S => {
                op!(builder, state, i_64_trunc_sat_f_64_s, i64);
            }
            Operator::I64TruncSatF64U => {
                op!(builder, state, i_64_trunc_sat_f_64_u, i64);
            }
            Operator::MemoryInit { data_index, mem } => {
                let (dest, src, len) = state.pop3()?;
                let mem = builder
                    .create(builder.iconst_32(mem as i32))
                    .result(0)?
                    .into();
                let segment = builder
                    .create(builder.iconst_32(data_index as i32))
                    .result(0)?
                    .into();

                builder.create(func::call(
                    &backend.ctx.mlir_context,
                    FlatSymbolRefAttribute::new(
                        &backend.ctx.mlir_context,
                        symbols::wasm::MEMORY_INIT,
                    ),
                    &[vm_ctx, mem, segment, dest, src, len],
                    &[],
                    builder.get_insert_location(),
                ));
            }
            Operator::DataDrop { data_index } => {
                let segment = builder
                    .create(builder.iconst_32(data_index as i32))
                    .result(0)?
                    .into();

                builder.create(func::call(
                    &backend.ctx.mlir_context,
                    FlatSymbolRefAttribute::new(
                        &backend.ctx.mlir_context,
                        symbols::wasm::DATA_DROP,
                    ),
                    &[vm_ctx, segment],
                    &[],
                    builder.get_insert_location(),
                ));
            }
            Operator::ElemDrop { elem_index } => {
                let segment = builder
                    .create(builder.iconst_32(elem_index as i32))
                    .result(0)?
                    .into();

                builder.create(func::call(
                    &backend.ctx.mlir_context,
                    FlatSymbolRefAttribute::new(
                        &backend.ctx.mlir_context,
                        symbols::wasm::ELEM_DROP,
                    ),
                    &[vm_ctx, segment],
                    &[],
                    builder.get_insert_location(),
                ));
            }
            Operator::MemoryCopy { dst_mem, src_mem } => {
                // ignored until we support multiple memories
                let _dst = dst_mem;
                let (dest_pos, src_pos, len) = state.pop3()?;

                if let Some(local_memory_index) = fcx
                    .wasm_module
                    .local_memory_index(MemoryIndex::from_u32(src_mem))
                {
                    let src_index = builder
                        .create(builder.iconst_32(local_memory_index.as_u32() as i32))
                        .result(0)?
                        .into();
                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::MEMORY_COPY,
                        ),
                        &[vm_ctx, src_index, dest_pos, src_pos, len],
                        &[],
                        builder.get_insert_location(),
                    ));
                } else {
                    let src_index = builder
                        .create(builder.iconst_32(src_mem as i32))
                        .result(0)?
                        .into();

                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::IMPORTED_MEMORY_COPY,
                        ),
                        &[vm_ctx, src_index, dest_pos, src_pos, len],
                        &[],
                        builder.get_insert_location(),
                    ));
                };
            }
            Operator::MemoryFill { mem } => {
                let (dst, val, len) = state.pop3()?;

                if let Some(local_memory_index) = fcx
                    .wasm_module
                    .local_memory_index(MemoryIndex::from_u32(mem))
                {
                    let mem_index = builder
                        .create(builder.iconst_32(local_memory_index.as_u32() as i32))
                        .result(0)?
                        .into();
                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::MEMORY_FILL,
                        ),
                        &[vm_ctx, mem_index, dst, val, len],
                        &[],
                        builder.get_insert_location(),
                    ));
                } else {
                    let mem_index = builder
                        .create(builder.iconst_32(mem as i32))
                        .result(0)?
                        .into();
                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::IMPORTED_MEMORY_FILL,
                        ),
                        &[vm_ctx, mem_index, dst, val, len],
                        &[],
                        builder.get_insert_location(),
                    ));
                };
            }
            Operator::TableInit { elem_index, table } => {
                let (dst, src, len) = state.pop3()?;
                let segment = builder
                    .create(builder.iconst_32(elem_index as i32))
                    .result(0)?
                    .into();
                let table = builder
                    .create(builder.iconst_32(table as i32))
                    .result(0)?
                    .into();
                builder.create(func::call(
                    &backend.ctx.mlir_context,
                    FlatSymbolRefAttribute::new(
                        &backend.ctx.mlir_context,
                        symbols::wasm::TABLE_INIT,
                    ),
                    &[vm_ctx, table, segment, dst, src, len],
                    &[],
                    builder.get_insert_location(),
                ));
            }
            Operator::TableCopy {
                dst_table,
                src_table,
            } => {
                let (dst, src, len) = state.pop3()?;
                let dst_table = builder
                    .create(builder.iconst_32(dst_table as i32))
                    .result(0)?
                    .into();
                let src_table = builder
                    .create(builder.iconst_32(src_table as i32))
                    .result(0)?
                    .into();

                builder.create(func::call(
                    &backend.ctx.mlir_context,
                    FlatSymbolRefAttribute::new(
                        &backend.ctx.mlir_context,
                        symbols::wasm::TABLE_COPY,
                    ),
                    &[vm_ctx, dst_table, src_table, dst, src, len],
                    &[],
                    builder.get_insert_location(),
                ));
            }
            Operator::TableFill { table } => {
                let table = builder
                    .create(builder.iconst_32(table as i32))
                    .result(0)?
                    .into();
                let (start, elem, len) = state.pop3()?;
                builder.create(func::call(
                    &backend.ctx.mlir_context,
                    FlatSymbolRefAttribute::new(
                        &backend.ctx.mlir_context,
                        symbols::wasm::TABLE_FILL,
                    ),
                    &[vm_ctx, table, start, elem, len],
                    &[],
                    builder.get_insert_location(),
                ));
            }
            Operator::TableGet { table } => {
                let table_index = builder
                    .create(builder.iconst_32(table as i32))
                    .result(0)?
                    .into();
                let elem = state.pop1()?;
                let op = if fcx
                    .wasm_module
                    .local_table_index(TableIndex::from_u32(table))
                    .is_some()
                {
                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::TABLE_GET,
                        ),
                        &[vm_ctx, table_index, elem],
                        &[builder.ptr_ty()],
                        builder.get_insert_location(),
                    ))
                } else {
                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::IMPORTED_TABLE_GET,
                        ),
                        &[vm_ctx, table_index, elem],
                        &[builder.ptr_ty()],
                        builder.get_insert_location(),
                    ))
                };
                let value = op.result(0)?.into();
                let op = builder.create(llvm::bitcast(
                    value,
                    type_to_mlir(
                        &backend.intrinsics,
                        &fcx.wasm_module
                            .tables
                            .get(TableIndex::from_u32(table))
                            .unwrap()
                            .ty,
                    ),
                    builder.unknown_loc(),
                ));
                let value = op.result(0)?.to_ctx_value();
                state.push1(value);
            }
            Operator::TableSet { table } => {
                let table_index = builder
                    .create(builder.iconst_32(table as i32))
                    .result(0)?
                    .into();
                let (elem, value) = state.pop2()?;
                if fcx
                    .wasm_module
                    .local_table_index(TableIndex::from_u32(table))
                    .is_some()
                {
                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::TABLE_SET,
                        ),
                        &[vm_ctx, table_index, elem, value],
                        &[],
                        builder.get_insert_location(),
                    ))
                } else {
                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::IMPORTED_TABLE_SET,
                        ),
                        &[vm_ctx, table_index, elem, value],
                        &[],
                        builder.get_insert_location(),
                    ))
                };
            }
            Operator::TableGrow { table } => {
                let (elem, delta) = state.pop2()?;
                let op = if let Some(local_table_index) = fcx
                    .wasm_module
                    .local_table_index(TableIndex::from_u32(table))
                {
                    let table_index = builder
                        .create(builder.iconst_32(local_table_index.as_u32() as i32))
                        .result(0)?
                        .into();
                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::TABLE_GROW,
                        ),
                        &[vm_ctx, elem, delta, table_index],
                        &[builder.i32_ty()],
                        builder.get_insert_location(),
                    ))
                } else {
                    let table_index = builder
                        .create(builder.iconst_32(table as i32))
                        .result(0)?
                        .into();
                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::IMPORTED_TABLE_GROW,
                        ),
                        &[vm_ctx, elem, delta, table_index],
                        &[builder.i32_ty()],
                        builder.get_insert_location(),
                    ))
                };
                let size = op.result(0)?.to_ctx_value();
                state.push1(size);
            }
            Operator::TableSize { table } => {
                let op = if let Some(local_table_index) = fcx
                    .wasm_module
                    .local_table_index(TableIndex::from_u32(table))
                {
                    let table_index = builder
                        .create(builder.iconst_32(local_table_index.as_u32() as i32))
                        .result(0)?
                        .into();
                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::TABLE_SIZE,
                        ),
                        &[vm_ctx, table_index],
                        &[builder.i32_ty()],
                        builder.get_insert_location(),
                    ))
                } else {
                    let table_index = builder
                        .create(builder.iconst_32(table as i32))
                        .result(0)?
                        .into();
                    builder.create(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(
                            &backend.ctx.mlir_context,
                            symbols::wasm::IMPORTED_TABLE_SIZE,
                        ),
                        &[vm_ctx, table_index],
                        &[builder.i32_ty()],
                        builder.get_insert_location(),
                    ))
                };
                let size = op.result(0)?.to_ctx_value();
                state.push1(size);
            }
            Operator::MemorySize { mem } => {
                let memory_index = MemoryIndex::from_u32(mem);
                let mem = builder
                    .create(builder.iconst_32(mem as i32))
                    .result(0)?
                    .into();
                let symbol = if fcx.wasm_module.local_memory_index(memory_index).is_some() {
                    symbols::wasm::MEMORY_SIZE
                } else {
                    symbols::wasm::IMPORTED_MEMORY_SIZE
                };
                let size = builder
                    .make(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(&backend.ctx.mlir_context, symbol),
                        &[vm_ctx, mem],
                        &[builder.i32_ty()],
                        builder.get_insert_location(),
                    ))?
                    .to_ctx_value();
                state.push1(size);
            }
            Operator::MemoryGrow { mem } => {
                let memory_index = MemoryIndex::from_u32(mem);
                let mem = builder
                    .create(builder.iconst_32(mem as i32))
                    .result(0)?
                    .into();
                let delta = state.pop1()?;
                let symbol = if fcx.wasm_module.local_memory_index(memory_index).is_some() {
                    symbols::wasm::MEMORY_GROW
                } else {
                    symbols::wasm::IMPORTED_MEMORY_GROW
                };
                let size = builder
                    .make(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(&backend.ctx.mlir_context, symbol),
                        &[vm_ctx, delta, mem],
                        &[builder.i32_ty()],
                        builder.get_insert_location(),
                    ))?
                    .to_ctx_value();
                state.push1(size);
            }
            Operator::MemoryAtomicNotify { ref memarg } => {
                let memory_index = MemoryIndex::from_u32(memarg.memory);
                let (dst, count) = state.pop2()?;
                let memory = builder.make(builder.iconst_32(memarg.memory as i32))?;
                let symbol = if fcx.wasm_module.local_memory_index(memory_index).is_some() {
                    symbols::wasm::MEMORY_NOTIFY
                } else {
                    symbols::wasm::IMPORTED_MEMORY_NOTIFY
                };
                let value = builder
                    .make(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(&backend.ctx.mlir_context, symbol),
                        &[vm_ctx, memory, dst, count],
                        &[builder.ptr_ty()],
                        builder.get_insert_location(),
                    ))?
                    .to_ctx_value();
                state.push1(value);
            }
            Operator::MemoryAtomicWait32 { ref memarg } => {
                let memory_index = MemoryIndex::from_u32(memarg.memory);
                let (dst, val) = state.pop2()?;
                let memory = builder.make(builder.iconst_32(memarg.memory as i32))?;
                let symbol = if fcx.wasm_module.local_memory_index(memory_index).is_some() {
                    symbols::wasm::MEMORY_WAIT32
                } else {
                    symbols::wasm::IMPORTED_MEMORY_WAIT32
                };
                let value = builder
                    .make(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(&backend.ctx.mlir_context, symbol),
                        &[vm_ctx, memory, dst, val],
                        &[builder.ptr_ty()],
                        builder.get_insert_location(),
                    ))?
                    .to_ctx_value();
                state.push1(value);
            }
            Operator::MemoryAtomicWait64 { ref memarg } => {
                let memory_index = MemoryIndex::from_u32(memarg.memory);
                let (dst, val) = state.pop2()?;
                let memory = builder.make(builder.iconst_32(memarg.memory as i32))?;
                let symbol = if fcx.wasm_module.local_memory_index(memory_index).is_some() {
                    symbols::wasm::MEMORY_WAIT64
                } else {
                    symbols::wasm::IMPORTED_MEMORY_WAIT64
                };
                let value = builder
                    .make(func::call(
                        &backend.ctx.mlir_context,
                        FlatSymbolRefAttribute::new(&backend.ctx.mlir_context, symbol),
                        &[vm_ctx, memory, dst, val],
                        &[builder.ptr_ty()],
                        builder.get_insert_location(),
                    ))?
                    .to_ctx_value();
                state.push1(value);
            }
            Operator::AtomicFence => {
                // Fence is a nop.
                //
                // Fence was added to preserve information about fences from
                // source languages. If in the future Wasm extends the memory
                // model, and if we hadn't recorded what fences used to be there,
                // it would lead to data races that weren't present in the
                // original source language.
            }
            Operator::I32AtomicLoad { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    4,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_opts(
                    effective_address,
                    builder.i32_ty(),
                    LoadStoreOptions {
                        align: Some(4),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ))?;
                state.push1(result.to_ctx_value());
                return Ok(block);
            }
            Operator::I64AtomicLoad { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    8,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_opts(
                    effective_address,
                    builder.i64_ty(),
                    LoadStoreOptions {
                        align: Some(8),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ))?;
                state.push1(result.to_ctx_value());
                return Ok(block);
            }
            Operator::I32AtomicLoad8U { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    1,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_opts(
                    effective_address,
                    builder.i8_ty(),
                    LoadStoreOptions {
                        align: Some(1),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ))?;
                let result = builder.make(arith::extui(
                    result,
                    builder.i32_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f32());
                return Ok(block);
            }
            Operator::I32AtomicLoad16U { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    2,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_opts(
                    effective_address,
                    builder.i16_ty(),
                    LoadStoreOptions {
                        align: Some(2),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ))?;
                let result = builder.make(arith::extui(
                    result,
                    builder.i32_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f32());
                return Ok(block);
            }
            Operator::I64AtomicLoad8U { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    1,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_opts(
                    effective_address,
                    builder.i8_ty(),
                    LoadStoreOptions {
                        align: Some(1),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ))?;
                let result = builder.make(arith::extui(
                    result,
                    builder.i64_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f64());
                return Ok(block);
            }
            Operator::I64AtomicLoad16U { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    2,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_opts(
                    effective_address,
                    builder.i16_ty(),
                    LoadStoreOptions {
                        align: Some(2),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ))?;
                let result = builder.make(arith::extui(
                    result,
                    builder.i64_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f32());
                return Ok(block);
            }
            Operator::I64AtomicLoad32U { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    4,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load_with_opts(
                    effective_address,
                    builder.i32_ty(),
                    LoadStoreOptions {
                        align: Some(4),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ))?;
                let result = builder.make(arith::extui(
                    result,
                    builder.i64_ty(),
                    builder.get_insert_location(),
                ))?;
                state.push1_extra(result.to_ctx_value(), ExtraInfo::arithmetic_f32());
                return Ok(block);
            }
            Operator::I32AtomicStore { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    4,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                builder.create(builder.store_with_opts(
                    value,
                    effective_address,
                    LoadStoreOptions {
                        align: Some(4),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ));
                return Ok(block);
            }
            Operator::I64AtomicStore { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    8,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                builder.create(builder.store_with_opts(
                    value,
                    effective_address,
                    LoadStoreOptions {
                        align: Some(8),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ));
                return Ok(block);
            }
            Operator::I32AtomicStore8 { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    1,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let value = builder.make(arith::trunci(
                    value,
                    builder.i8_ty(),
                    builder.get_insert_location(),
                ))?;
                builder.create(builder.store_with_opts(
                    value,
                    effective_address,
                    LoadStoreOptions {
                        align: Some(1),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ));
                return Ok(block);
            }
            Operator::I32AtomicStore16 { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    2,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let value = builder.make(arith::trunci(
                    value,
                    builder.i16_ty(),
                    builder.get_insert_location(),
                ))?;
                builder.create(builder.store_with_opts(
                    value,
                    effective_address,
                    LoadStoreOptions {
                        align: Some(2),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ));
                return Ok(block);
            }
            Operator::I64AtomicStore8 { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    1,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let value = builder.make(arith::trunci(
                    value,
                    builder.i8_ty(),
                    builder.get_insert_location(),
                ))?;
                builder.create(builder.store_with_opts(
                    value,
                    effective_address,
                    LoadStoreOptions {
                        align: Some(1),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ));
                return Ok(block);
            }
            Operator::I64AtomicStore16 { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    2,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let value = builder.make(arith::trunci(
                    value,
                    builder.i16_ty(),
                    builder.get_insert_location(),
                ))?;
                builder.make(builder.store_with_opts(
                    value,
                    effective_address,
                    LoadStoreOptions {
                        align: Some(2),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ))?;
                return Ok(block);
            }
            Operator::I64AtomicStore32 { ref memarg } => {
                let (offset, value) = state.pop2()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    4,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let value = builder.make(arith::trunci(
                    value,
                    builder.i32_ty(),
                    builder.get_insert_location(),
                ))?;
                builder.create(builder.store_with_opts(
                    value,
                    effective_address,
                    LoadStoreOptions {
                        align: Some(4),
                        volatile: fcx.is_static_memory_index(memory_index),
                        ordering: Some(AtomicOrdering::SequentiallyConsistent),
                        ..Default::default()
                    },
                ));
                return Ok(block);
            }
            Operator::I32AtomicRmwAdd { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Add, i32
                );
            }
            Operator::I64AtomicRmwAdd { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Add, i64
                );
            }
            Operator::I32AtomicRmw8AddU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Add, i32, i8
                );
            }
            Operator::I32AtomicRmw16AddU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Add, i32, i16
                );
            }
            Operator::I64AtomicRmw8AddU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Add, i64, i8
                );
            }
            Operator::I64AtomicRmw16AddU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Add, i64, i16
                );
            }
            Operator::I64AtomicRmw32AddU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Add, i64, i32
                );
            }
            Operator::I32AtomicRmwSub { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Sub, i32
                );
            }
            Operator::I64AtomicRmwSub { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Sub, i64
                );
            }
            Operator::I32AtomicRmw8SubU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Sub, i32, i8
                );
            }
            Operator::I32AtomicRmw16SubU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Sub, i32, i16
                );
            }
            Operator::I64AtomicRmw8SubU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Sub, i64, i8
                );
            }
            Operator::I64AtomicRmw16SubU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Sub, i64, i16
                );
            }
            Operator::I64AtomicRmw32SubU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Sub, i64, i32
                );
            }
            Operator::I32AtomicRmwAnd { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, And, i32
                );
            }
            Operator::I64AtomicRmwAnd { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, And, i64
                );
            }
            Operator::I32AtomicRmw8AndU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, And, i32, i8
                );
            }
            Operator::I32AtomicRmw16AndU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, And, i32, i16
                );
            }
            Operator::I64AtomicRmw8AndU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, And, i64, i8
                );
            }
            Operator::I64AtomicRmw16AndU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, And, i64, i16
                );
            }
            Operator::I64AtomicRmw32AndU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, And, i64, i32
                );
            }
            Operator::I32AtomicRmwOr { ref memarg } => {
                atomicrmw_op!(builder, state, memarg, fcx, backend, region, block, Or, i32);
            }
            Operator::I64AtomicRmwOr { ref memarg } => {
                atomicrmw_op!(builder, state, memarg, fcx, backend, region, block, Or, i64);
            }
            Operator::I32AtomicRmw8OrU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Or, i32, i8
                );
            }
            Operator::I32AtomicRmw16OrU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Or, i32, i16
                );
            }
            Operator::I64AtomicRmw8OrU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Or, i64, i8
                );
            }
            Operator::I64AtomicRmw16OrU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Or, i64, i16
                );
            }
            Operator::I64AtomicRmw32OrU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Or, i64, i32
                );
            }
            Operator::I32AtomicRmwXor { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xor, i32
                );
            }
            Operator::I64AtomicRmwXor { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xor, i64
                );
            }
            Operator::I32AtomicRmw8XorU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xor, i32, i8
                );
            }
            Operator::I32AtomicRmw16XorU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xor, i32, i16
                );
            }
            Operator::I64AtomicRmw8XorU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xor, i64, i8
                );
            }
            Operator::I64AtomicRmw16XorU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xor, i64, i16
                );
            }
            Operator::I64AtomicRmw32XorU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xor, i64, i32
                );
            }
            Operator::I32AtomicRmwXchg { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xchg, i32
                );
            }
            Operator::I64AtomicRmwXchg { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xchg, i64
                );
            }
            Operator::I32AtomicRmw8XchgU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xchg, i32, i8
                );
            }
            Operator::I32AtomicRmw16XchgU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xchg, i32, i16
                );
            }
            Operator::I64AtomicRmw8XchgU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xchg, i64, i8
                );
            }
            Operator::I64AtomicRmw16XchgU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xchg, i64, i16
                );
            }
            Operator::I64AtomicRmw32XchgU { ref memarg } => {
                atomicrmw_op!(
                    builder, state, memarg, fcx, backend, region, block, Xchg, i64, i32
                );
            }
            Operator::I32AtomicRmwCmpxchg { ref memarg } => {
                atomicrmw_cmpxchg_op!(
                    builder, state, memarg, fcx, backend, region, block, i32, i32_ty
                );
            }
            Operator::I64AtomicRmwCmpxchg { ref memarg } => {
                atomicrmw_cmpxchg_op!(
                    builder, state, memarg, fcx, backend, region, block, i64, i64_ty
                );
            }
            Operator::I32AtomicRmw8CmpxchgU { ref memarg } => {
                atomicrmw_cmpxchg_op!(
                    builder,
                    state,
                    memarg,
                    fcx,
                    backend,
                    region,
                    block,
                    i32,
                    i32_ty,
                    i8_ty,
                    arithmetic_f32
                );
            }
            Operator::I32AtomicRmw16CmpxchgU { ref memarg } => {
                atomicrmw_cmpxchg_op!(
                    builder,
                    state,
                    memarg,
                    fcx,
                    backend,
                    region,
                    block,
                    i32,
                    i32_ty,
                    i16_ty,
                    arithmetic_f32
                );
            }
            Operator::I64AtomicRmw8CmpxchgU { ref memarg } => {
                atomicrmw_cmpxchg_op!(
                    builder,
                    state,
                    memarg,
                    fcx,
                    backend,
                    region,
                    block,
                    i64,
                    i64_ty,
                    i8_ty,
                    arithmetic_f64
                );
            }
            Operator::I64AtomicRmw16CmpxchgU { ref memarg } => {
                atomicrmw_cmpxchg_op!(
                    builder,
                    state,
                    memarg,
                    fcx,
                    backend,
                    region,
                    block,
                    i64,
                    i64_ty,
                    i16_ty,
                    arithmetic_f64
                );
            }
            Operator::I64AtomicRmw32CmpxchgU { ref memarg } => {
                atomicrmw_cmpxchg_op!(
                    builder,
                    state,
                    memarg,
                    fcx,
                    backend,
                    region,
                    block,
                    i64,
                    i64_ty,
                    i32_ty,
                    arithmetic_f64
                );
            }
            Operator::V128Load { ref memarg } => {
                let offset = state.pop1()?;
                let memory_index = MemoryIndex::from_u32(0);
                let (block, effective_address) = Self::resolve_memory_ptr(
                    memory_index,
                    memarg,
                    offset,
                    16,
                    fcx,
                    backend.ctx,
                    region,
                    block,
                )?;
                let builder = OpBuilder::new_with_block(builder.context(), block);
                let result = builder.make(builder.load(effective_address, builder.i128_ty()))?;
                state.push1(result.to_ctx_value());
                return Ok(block);
            }
            Operator::CallIndirect {
                type_index,
                table_index,
            } => {
                let sigindex = SignatureIndex::from_u32(type_index);
                let func_type = &fcx.wasm_module.signatures[sigindex];
                let expected_dynamic_sigindex = fcx.ctx.dynamic_sigindex(sigindex)?;
                let (table_base, table_bound) = fcx.ctx.table(TableIndex::from_u32(table_index))?;
                let func_index = state.pop1()?;
                let ctx = &backend.ctx.mlir_context;

                let index_in_bounds =
                    builder.make(builder.icmp(IntCC::UnsignedLessThan, func_index, table_bound))?;
                let in_bounds_continue_block = region.append_block(Block::new(&[]));
                let not_in_bounds_block = region.append_block(Block::new(&[]));
                builder.create(cf::cond_br(
                    ctx,
                    index_in_bounds,
                    &in_bounds_continue_block,
                    &not_in_bounds_block,
                    &[],
                    &[],
                    builder.get_insert_location(),
                ));
                // Raise the table access OOB error
                {
                    let builder = OpBuilder::new_with_block(ctx, not_in_bounds_block);
                    trap(
                        &builder,
                        TrapCode::TableAccessOutOfBounds,
                        in_bounds_continue_block,
                    )?;
                }
                let builder = OpBuilder::new_with_block(ctx, in_bounds_continue_block);
                // We assume the table has the `funcref` (pointer to `anyfunc`) element type.
                let funcref_ptr = builder
                    .make(builder.inbounds_gep_dynamic(
                        table_base,
                        &[func_index],
                        builder.ptr_ty(),
                        builder.ptr_ty(),
                    ))?
                    .to_ctx_value();
                // A funcref (pointer to `anyfunc`)
                let anyfunc_struct_ptr = builder
                    .make(builder.load(funcref_ptr, builder.ptr_ty()))?
                    .to_ctx_value();

                let funcref_is_null = is_zero(&builder, anyfunc_struct_ptr)?;
                let funcref_continue_deref_block = region.append_block(Block::new(&[]));
                let funcref_is_null_block = region.append_block(Block::new(&[]));
                builder.create(cf::cond_br(
                    ctx,
                    funcref_is_null,
                    &funcref_is_null_block,
                    &funcref_continue_deref_block,
                    &[],
                    &[],
                    builder.get_insert_location(),
                ));
                // Trap if we're trying to call a null funcref
                {
                    let builder = OpBuilder::new_with_block(ctx, funcref_is_null_block);
                    trap(
                        &builder,
                        TrapCode::IndirectCallToNull,
                        funcref_continue_deref_block,
                    )?;
                }
                let builder = OpBuilder::new_with_block(ctx, funcref_continue_deref_block);
                // Load things from the anyfunc data structure.
                let func_ptr_ptr = builder
                    .make(builder.gep(
                        anyfunc_struct_ptr,
                        fcx.ctx.offsets.vmcaller_checked_anyfunc_func_ptr() as usize,
                        builder.i8_ty(),
                        builder.ptr_ty(),
                    ))?
                    .to_ctx_value();
                let sigindex_ptr = builder.make(builder.gep(
                    anyfunc_struct_ptr,
                    fcx.ctx.offsets.vmcaller_checked_anyfunc_type_index() as usize,
                    builder.i8_ty(),
                    builder.ptr_ty(),
                ))?;
                let ctx_ptr_ptr = builder
                    .make(builder.gep(
                        anyfunc_struct_ptr,
                        fcx.ctx.offsets.vmcaller_checked_anyfunc_vmctx() as usize,
                        builder.i8_ty(),
                        builder.ptr_ty(),
                    ))?
                    .to_ctx_value();
                let func_ptr = builder
                    .make(builder.load(func_ptr_ptr, builder.ptr_ty()))?
                    .to_ctx_value();
                let ctx_ptr = builder
                    .make(builder.load(ctx_ptr_ptr, builder.ptr_ty()))?
                    .to_ctx_value();

                let elem_not_initialized = is_zero(&builder, func_ptr)?;
                let continue_block = region.append_block(Block::new(&[]));
                let elem_not_initialized_block = region.append_block(Block::new(&[]));
                builder.create(cf::cond_br(
                    ctx,
                    elem_not_initialized,
                    &elem_not_initialized_block,
                    &continue_block,
                    &[],
                    &[],
                    builder.get_insert_location(),
                ));
                // Check if the table element is initialized.
                {
                    let builder = OpBuilder::new_with_block(ctx, elem_not_initialized_block);
                    trap(&builder, TrapCode::TableAccessOutOfBounds, continue_block)?;
                }
                let builder = OpBuilder::new_with_block(ctx, continue_block);
                let found_dynamic_sigindex =
                    builder.make(builder.load(sigindex_ptr, builder.i32_ty()))?;
                let sigindices_not_equal = builder.make(builder.icmp(
                    IntCC::NotEqual,
                    expected_dynamic_sigindex,
                    found_dynamic_sigindex,
                ))?;
                let continue_block = region.append_block(Block::new(&[]));
                let sigindices_notequal_block = region.append_block(Block::new(&[]));
                builder.create(cf::cond_br(
                    ctx,
                    sigindices_not_equal,
                    &sigindices_notequal_block,
                    &continue_block,
                    &[],
                    &[],
                    builder.get_insert_location(),
                ));
                // Check if the signature id is correct.
                {
                    let builder = OpBuilder::new_with_block(ctx, sigindices_notequal_block);
                    trap(&builder, TrapCode::BadSignature, continue_block)?;
                }
                let builder = OpBuilder::new_with_block(ctx, continue_block);
                let return_types = func_type
                    .results()
                    .iter()
                    .map(|ty| type_to_mlir(&backend.intrinsics, ty))
                    .collect::<Vec<Type>>();
                // Multiple return values is not supported, because they have different ABIs on different platforms.
                if return_types.len() > 1 {
                    return Err(CompileError::Codegen(
                        "Multiple return values in the call_indirect op is not supported"
                            .to_string(),
                    )
                    .into());
                }
                let ret_ty = if !return_types.is_empty() {
                    return_types[0]
                } else {
                    builder.intrinsics.void_ty
                };
                let args = state.popn_save_extra(func_type.params().len())?;
                let args = args.iter().map(|p| p.0).collect::<Vec<Value<'_, '_>>>();
                let args = std::iter::once(ctx_ptr)
                    .chain(args.iter().copied())
                    .collect::<Vec<Value<'_, '_>>>();
                let result = builder.make(builder.indirect_call(ret_ty, func_ptr, &args)?)?;
                state.push1(result.to_ctx_value());
                return Ok(continue_block);
            }
            _ => {
                return Err(
                    CompileError::Codegen(format!("Operator {:?} unimplemented", op)).into(),
                );
            }
        }
        Ok(block)
    }

    /// Finalizes the translation of a WebAssembly function by appending a return operation.
    ///
    /// This function is responsible for handling the final steps in the translation of a WebAssembly
    /// function to MLIR, including retrieving the result values from the backend's state and appending
    /// them as a return operation to the provided block. The function also ensures that the result types
    /// conform to both the WebAssembly function type and the MLIR function type.
    ///
    /// # Parameters
    /// - `backend`: A mutable reference to the `WASMBackend` which manages the context and translation state.
    /// - `block`: A reference to the block where the return operation will be appended.
    /// - `wasm_fn_type`: A reference to the WebAssembly `FunctionType` which provides the expected result types.
    /// - `fn_type`: A reference to the MLIR `FunctionType` which defines the corresponding MLIR-level function signature.
    ///
    /// # Returns
    /// A `Result` indicating whether the operation succeeded. On success, `Ok(())` is returned. On failure, an error is returned.
    ///
    /// # Errors
    /// This function may return an error if there is a failure in retrieving or processing the result values from the backend state.
    pub fn finalize<'c>(
        backend: &mut WASMBackend<'c>,
        block: BlockRef<'c, '_>,
        wasm_fn_type: &FunctionType,
    ) -> Result<()> {
        debug_assert!(block.argument_count() == wasm_fn_type.results().len());
        let mut results = vec![];
        for i in 0..block.argument_count() {
            results.push(block.argument(i)?.into());
        }
        block.append_operation(func::r#return(&results, backend.intrinsics.unknown_loc));
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_memory_ptr<'c, 'a>(
        memory_index: MemoryIndex,
        memarg: &MemArg,
        var_offset: Value<'c, 'a>,
        value_size: usize,
        fcx: &mut FunctionCodeCtx<'c, 'a>,
        ctx: &'c Context,
        region: &'c Region<'c>,
        block: BlockRef<'c, 'a>,
    ) -> Result<(BlockRef<'c, 'a>, Value<'c, 'a>)>
    where
        'a: 'c,
    {
        let ctx = &ctx.mlir_context;
        let builder = OpBuilder::new_with_block(ctx, block);
        let location = builder.get_insert_location();
        // Compute the offset into the storage.
        let imm_offset = builder.make(builder.iconst_64(memarg.offset as i64))?;
        let var_offset = if builder.int_ty_width(var_offset.r#type())? == 64 {
            var_offset
        } else {
            builder.make(arith::extui(var_offset, builder.i64_ty(), location))?
        };
        let offset = builder.make(arith::addi(var_offset, imm_offset, location))?;
        // Look up the memory base (as pointer) and bounds (as unsigned integer).
        let (base_ptr, block) = match fcx.ctx.memory(memory_index, fcx.memory_styles)? {
            MemoryCache::Dynamic {
                ptr_to_base_ptr,
                ptr_to_current_length,
            } => {
                let value_size_v = builder.make(builder.iconst_64(value_size as i64))?;
                let load_offset_end = builder.make(arith::addi(offset, value_size_v, location))?;

                let mut current_length =
                    builder.make(builder.load(ptr_to_current_length, builder.isize_ty()))?;
                if builder.int_ty_width(current_length.r#type())? != 64 {
                    current_length =
                        builder.make(arith::extui(current_length, builder.i64_ty(), location))?;
                }

                let ptr_in_bounds = builder.make(builder.icmp(
                    IntCC::UnsignedLessThan,
                    load_offset_end,
                    current_length,
                ))?;
                // Check memory out of index error.
                let in_bounds_continue_block = region.append_block(Block::new(&[]));
                let not_in_bounds_block = region.append_block(Block::new(&[]));
                builder.create(cf::cond_br(
                    ctx,
                    ptr_in_bounds,
                    &in_bounds_continue_block,
                    &not_in_bounds_block,
                    &[],
                    &[],
                    location,
                ));
                // Raise the memory OOB error
                {
                    let builder = OpBuilder::new_with_block(ctx, not_in_bounds_block);
                    trap(
                        &builder,
                        TrapCode::HeapAccessOutOfBounds,
                        in_bounds_continue_block,
                    )?;
                }
                (
                    builder
                        .make(builder.load(ptr_to_base_ptr, builder.ptr_ty()))?
                        .to_ctx_value(),
                    in_bounds_continue_block,
                )
            }
            MemoryCache::Static { base_ptr } => {
                if fcx.static_memory_bound_check {
                    let value_size_v = builder.make(builder.iconst_64(value_size as i64))?;
                    let load_offset_end =
                        builder.make(arith::addi(offset, value_size_v, location))?;
                    let symbol = if fcx.wasm_module.local_memory_index(memory_index).is_some() {
                        symbols::wasm::MEMORY_SIZE
                    } else {
                        symbols::wasm::IMPORTED_MEMORY_SIZE
                    };
                    let mem = builder
                        .create(builder.iconst_32(memory_index.as_u32() as i32))
                        .result(0)?
                        .into();
                    let page_size = builder
                        .make(func::call(
                            ctx,
                            FlatSymbolRefAttribute::new(ctx, symbol),
                            &[fcx.ctx.vm_ctx, mem],
                            &[builder.i32_ty()],
                            location,
                        ))?
                        .to_ctx_value();
                    let page_size =
                        builder.make(arith::extui(page_size, builder.i64_ty(), location))?;
                    // WASM page sizes are fixed to be 64KiB = 65536
                    let current_length = builder.make(arith::muli(
                        page_size,
                        builder.make(builder.iconst_64(WASM_PAGE_SIZE as i64))?,
                        location,
                    ))?;
                    let ptr_in_bounds = builder.make(builder.icmp(
                        IntCC::UnsignedLessThan,
                        load_offset_end,
                        current_length,
                    ))?;
                    // Check memory out of index error.
                    let in_bounds_continue_block = region.append_block(Block::new(&[]));
                    let not_in_bounds_block = region.append_block(Block::new(&[]));
                    builder.create(cf::cond_br(
                        ctx,
                        ptr_in_bounds,
                        &in_bounds_continue_block,
                        &not_in_bounds_block,
                        &[],
                        &[],
                        builder.get_insert_location(),
                    ));
                    // Raise the memory OOB error
                    {
                        let builder = OpBuilder::new_with_block(ctx, not_in_bounds_block);
                        trap(
                            &builder,
                            TrapCode::HeapAccessOutOfBounds,
                            in_bounds_continue_block,
                        )?;
                    }
                    (base_ptr, in_bounds_continue_block)
                } else {
                    (base_ptr, block)
                }
            }
        };
        let builder = OpBuilder::new_with_block(ctx, block);
        let memory_ptr = builder.make(builder.gep_dynamic(
            base_ptr,
            &[offset],
            builder.i8_ty(),
            builder.ptr_ty(),
        ))?;
        Ok((block, unsafe { Value::from_raw(memory_ptr.to_raw()) }))
    }
}
