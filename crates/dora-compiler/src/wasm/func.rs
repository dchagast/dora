use super::WASMCompileOptions;
use super::backend::{WASMBackend, gas_limit};
use super::code::{FunctionCodeCtx, FunctionCodeGenerator};
use super::intrinsics::CtxType;
use super::ty::{type_to_mlir, type_to_mlir_zero_attribute};
use super::{intrinsics::WASMIntrinsics, ty::func_type_to_mlir};
use crate::context::Context;
use crate::conversion::builder::OpBuilder;
use crate::errors::Result;
use crate::state::PhiValue;
use melior::dialect::{arith, cf, func};
use melior::ir::attribute::{Attribute, StringAttribute, TypeAttribute};
use melior::ir::{Block, Identifier, Location, Operation, Region};
use smallvec::SmallVec;
use wasmer::ExportIndex;
use wasmer_compiler::types::symbols::{Symbol, SymbolRegistry};
use wasmer_compiler::{
    FunctionBinaryReader, FunctionBodyData, MiddlewareBinaryReader, ModuleMiddlewareChain,
    ModuleTranslationState, wptype_to_type,
};
use wasmer_types::entity::PrimaryMap;
use wasmer_types::{
    FunctionIndex, LocalFunctionIndex, MemoryIndex, MemoryStyle, ModuleInfo, TableIndex, TableStyle,
};

/// Responsible for translating WebAssembly functions into the target intermediate representation
/// (IR). The `FuncTranslator` struct holds the necessary context and configuration to guide
/// the function translation process.
///
/// # Fields:
/// - `context`: The `Context` in which the translation takes place. This contains the environment
///   and state required for building and managing the intermediate representation (IR), such as
///   the mapping of WebAssembly operations to target operations.
/// - `config`: The `Config` structure that provides the configuration options, such as optimization
///   levels and verifier settings, to control how the function is translated.
///
/// # Example Usage:
/// ```no_check
/// let func_translator = FuncTranslator {
///     context: /* Context instance */,
/// };
///
/// // Use func_translator to translate WebAssembly functions into the target IR.
/// func_translator.translate_function(/* FunctionCodeCtx, MLIRModule, etc. */);
/// ```
///
/// # Notes:
/// - The `FuncTranslator` struct is a core component in the WebAssembly-to-IR translation process.
///   It takes function-level WebAssembly code and translates it into a lower-level or intermediate
///   representation (IR) that can be further optimized and executed.
/// - The `context` field holds all necessary data structures and state required to manage and
///   construct the IR during the translation phase.
/// - The `config` field allows users to customize the translation process, enabling or disabling
///   features like optimization passes and code validation.
pub(crate) struct FuncTranslator;

impl FuncTranslator {
    /// Translates a WebAssembly function to an MLIR module.
    ///
    /// This method performs the core translation of the WebAssembly function body into
    /// an MLIR-based function. It converts WebAssembly types, memory, and table styles
    /// into their corresponding MLIR types and operations.
    ///
    /// # Parameters
    /// - `module`: The MLIR module to append the translated function.
    /// - `wasm_module`: A reference to the WebAssembly `ModuleInfo`.
    /// - `module_translation`: A reference to the module translation state.
    /// - `local_func_index`: A reference to the local function index within the WebAssembly module.
    /// - `function_body`: The body data of the WebAssembly function.
    /// - `memory_styles`: A map containing memory styles for each memory index in the module.
    /// - `table_styles`: A map containing table styles for each table index in the module.
    /// - `symbol_registry`: A reference to a `SymbolRegistry` used for symbol resolution.
    ///
    /// # Returns
    /// A `Result` indicating success or failure during the translation process.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn translate<'c>(
        context: &'c Context,
        opts: &'c WASMCompileOptions,
        wasm_module: &'c ModuleInfo,
        module_translation: &'c ModuleTranslationState,
        local_func_index: &'c LocalFunctionIndex,
        function_body: &'c FunctionBodyData,
        memory_styles: &'c PrimaryMap<MemoryIndex, MemoryStyle>,
        table_styles: &'c PrimaryMap<TableIndex, TableStyle>,
        symbol_registry: &'c dyn SymbolRegistry,
    ) -> Result<Operation<'c>> {
        let func_index = wasm_module.func_index(*local_func_index);
        let (function_name, is_export_function) =
            Self::get_func_name(wasm_module, local_func_index, &func_index, symbol_registry);
        let wasm_fn_type = wasm_module
            .signatures
            .get(wasm_module.functions[func_index])
            .unwrap();
        let intrinsics = WASMIntrinsics::declare(context);
        let func_type = func_type_to_mlir(context, &intrinsics, wasm_fn_type);
        let func = func::func(
            &context.mlir_context,
            StringAttribute::new(&context.mlir_context, &function_name),
            TypeAttribute::new(func_type.into()),
            {
                let region = Region::new();
                {
                    let setup_block =
                        region.append_block(Self::block_from_func_ty(context, &func_type)?);
                    let mut reader = MiddlewareBinaryReader::new_with_offset(
                        function_body.data,
                        function_body.module_offset,
                    );
                    reader.set_middleware_chain(
                        opts.middlewares
                            .generate_function_middleware_chain(*local_func_index),
                    );
                    let builder = OpBuilder::new_with_block(&context.mlir_context, setup_block);
                    // Setup function parameters
                    let mut params = vec![];
                    for idx in 0..wasm_fn_type.params().len() {
                        let ty = wasm_fn_type.params()[idx];
                        let ty = type_to_mlir(&intrinsics, &ty);
                        // The first argument is the system context pointer.
                        let value = setup_block.argument(idx + 1)?;
                        let value_ptr = builder.make(builder.alloca(ty)?)?;
                        builder.create(builder.store(value.into(), value_ptr));
                        params.push((ty, value_ptr));
                    }
                    // Setup function local variables
                    let mut locals = vec![];
                    let num_locals = reader.read_local_count()?;
                    for _ in 0..num_locals {
                        let (count, ty) = reader.read_local_decl()?;
                        let ty = wptype_to_type(ty)?;
                        let mlir_ty = type_to_mlir(&intrinsics, &ty);
                        let mlir_val =
                            type_to_mlir_zero_attribute(&context.mlir_context, &intrinsics, ty);
                        let value = setup_block
                            .append_operation(arith::constant(
                                &context.mlir_context,
                                mlir_val,
                                Location::unknown(&context.mlir_context),
                            ))
                            .result(0)?
                            .into();
                        for _ in 0..count {
                            let value_ptr = builder.make(builder.alloca(mlir_ty)?)?;
                            builder.create(builder.store(value, value_ptr));
                            locals.push((mlir_ty, value_ptr));
                        }
                    }
                    // Setup gas meter counter context value and out of gas trap block
                    let gas_counter_ptr = if opts.gas_metering {
                        let gas_limit = gas_limit(&builder)?;
                        let gas_counter_ptr = builder.make(builder.alloca(builder.i64_ty())?)?;
                        builder.create(builder.store(gas_limit, gas_counter_ptr));
                        Some(gas_counter_ptr)
                    } else {
                        None
                    };
                    let mut params_locals = params.clone();
                    params_locals.extend(locals.iter().cloned());
                    let mut backend = WASMBackend::new(context);
                    let phis: SmallVec<[PhiValue<'_>; 1]> = wasm_fn_type
                        .results()
                        .iter()
                        .map(|wasm_ty| {
                            (
                                type_to_mlir(&backend.intrinsics, wasm_ty),
                                intrinsics.unknown_loc,
                            )
                        })
                        .collect();
                    let code_start_lock = region.append_block(Block::new(&[]));
                    let br_op = builder.create(cf::br(
                        &code_start_lock,
                        &[],
                        builder.get_insert_location(),
                    ));
                    let return_block = region.append_block(Block::new(&phis));
                    backend.state.push_block(return_block, phis);
                    let vm_ctx = setup_block.argument(0)?;
                    let mut fcx = FunctionCodeCtx {
                        locals: params_locals,
                        ctx: CtxType::new(
                            vm_ctx.into(),
                            wasm_module,
                            OpBuilder::new_with_op(&context.mlir_context, br_op),
                        ),
                        unreachable_depth: 0,
                        memory_styles,
                        _table_styles: table_styles,
                        module_translation,
                        wasm_module,
                        symbol_registry,
                        static_memory_bound_check: opts.static_memory_bound_check,
                    };
                    let mut last_block = code_start_lock;
                    while backend.state.has_control_frames() {
                        // The next WASM operator
                        let pos = reader.current_position() as u32;
                        let op = reader.read_operator()?;
                        // Op translation
                        let end_block = FunctionCodeGenerator::translate_op(
                            op,
                            &mut fcx,
                            &mut backend,
                            &region,
                            last_block,
                            gas_counter_ptr,
                            pos,
                        )?;
                        last_block = end_block;
                    }
                    FunctionCodeGenerator::finalize(&mut backend, last_block, wasm_fn_type)?;
                }
                region
            },
            &if is_export_function {
                vec![
                    (
                        Identifier::new(&context.mlir_context, "sym_visibility"),
                        StringAttribute::new(&context.mlir_context, "public").into(),
                    ),
                    (
                        Identifier::new(&context.mlir_context, "llvm.emit_c_interface"),
                        Attribute::unit(&context.mlir_context),
                    ),
                ]
            } else {
                vec![]
            },
            intrinsics.unknown_loc,
        );
        Ok(func)
    }

    /// Creates a new MLIR block based on the function type.
    ///
    /// This method constructs an MLIR block with the necessary argument types based on the given
    /// MLIR function type. Each input argument is added to the block with its corresponding type
    /// and location.
    ///
    /// # Parameters
    /// - `ty`: A reference to the MLIR `FunctionType` that defines the input types for the block.
    ///
    /// # Returns
    /// A `Result` containing the new `Block` on success, or an error on failure.
    pub fn block_from_func_ty<'c>(
        context: &'c Context,
        ty: &melior::ir::r#type::FunctionType<'c>,
    ) -> Result<Block<'c>> {
        let block = Block::new(&[]);
        let count = ty.input_count();
        for i in 0..count {
            let ty = ty.input(i)?;
            block.add_argument(ty, Location::unknown(&context.mlir_context));
        }
        Ok(block)
    }

    pub(crate) fn get_func_name<'c>(
        wasm_module: &'c ModuleInfo,
        local_func_index: &'c LocalFunctionIndex,
        func_index: &'c FunctionIndex,
        symbol_registry: &'c dyn SymbolRegistry,
    ) -> (String, bool) {
        let mut is_export_function = false;
        // Find name in export functions
        let mut ret_name = String::new();
        for (name, export_index) in &wasm_module.exports {
            if let ExportIndex::Function(index) = export_index {
                if *index == *func_index {
                    ret_name = name.clone();
                    is_export_function = true;
                    break;
                }
            }
        }
        let function_name = if !ret_name.is_empty() {
            ret_name
        } else {
            match wasm_module.function_names.get(func_index) {
                Some(name) => name.to_string(),
                None => symbol_registry.symbol_to_name(Symbol::LocalFunction(*local_func_index)),
            }
        };
        (function_name, is_export_function)
    }
}
