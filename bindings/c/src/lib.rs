// Copyright 2024 The Dora Authors.
// Licensed under the Apache License, Version 2.0.

use dora::{
    build_artifact,
    primitives::{Address, Bytecode, Bytes, Bytes32, SpecId},
    runtime::{
        ExitStatusCode,
        artifact::{Artifact, SymbolArtifact},
        call::CallKind,
        context::{Contract, RuntimeContext},
        db::MemoryDB,
    },
};
use evmc_declare::evmc_declare_vm;
use evmc_sys::{evmc_address, evmc_call_kind};
use evmc_vm::*;
use host::EvmcDelegateHost;
use lazy_static::lazy_static;
use rustc_hash::FxHashMap;
use std::sync::Mutex;

mod host;

#[cfg(test)]
mod tests;

lazy_static! {
    static ref ARTIFACTS: Mutex<FxHashMap<Bytes, SymbolArtifact>> =
        Mutex::new(FxHashMap::default());
}

#[evmc_declare_vm("dora", "evm, ewasm, precompiles", "12.0.0")]
pub struct DoraVM {}

impl EvmcVm for DoraVM {
    fn init() -> Self {
        Self {}
    }

    fn execute<'a>(
        &self,
        revision: Revision,
        code: &'a [u8],
        message: &'a ExecutionMessage,
        context: Option<&'a mut ExecutionContext<'a>>,
    ) -> ExecutionResult {
        let Some(context) = context else {
            return ExecutionResult::failure();
        };
        let spec_id = evmc_revision_to_spec_id(revision);
        let contract = Contract {
            input: message.input().cloned().unwrap_or_else(Vec::new).into(),
            code: Bytecode::new_raw(code.to_owned().into()),
            hash: None,
            target_address: evmc_address_to_address(message.recipient()),
            code_address: evmc_address_to_address(message.code_address()),
            caller: evmc_address_to_address(message.sender()),
            call_value: Bytes32::from_be_bytes(message.value().bytes).into_u256(),
        };
        let mut host = EvmcDelegateHost::new(context, spec_id);
        let runtime_context = RuntimeContext::new(
            contract,
            message.depth() as usize,
            false,
            matches!(message.kind(), evmc_call_kind::EVMC_EOFCREATE),
            &mut host,
            spec_id,
            message.gas() as u64,
        );
        let mut artifacts = ARTIFACTS.lock().unwrap();
        let artifact = if let Some(artifact) =
            artifacts.get(runtime_context.contract.code.original_byte_slice().as_ref())
        {
            artifact.clone()
        } else {
            let Ok(artifact) = build_artifact::<MemoryDB>(
                &runtime_context.contract.code,
                runtime_context.inner.spec_id,
            ) else {
                return ExecutionResult::failure();
            };
            artifacts.insert(
                runtime_context.contract.code.original_bytes(),
                artifact.clone(),
            );
            artifact
        };
        drop(artifacts);
        let Ok(result) = artifact.execute(runtime_context) else {
            return ExecutionResult::failure();
        };
        // Runtime errors
        let exit_code = result.status;
        if exit_code.is_ok() {
            ExecutionResult::success(
                result.gas_remaining as i64,
                result.gas_refunded,
                Some(&result.output),
            )
        } else if exit_code.is_revert() {
            ExecutionResult::new(
                status_to_evmc_status(exit_code),
                result.gas_remaining as i64,
                0,
                Some(&result.output),
            )
        } else if exit_code.is_error() {
            ExecutionResult::new(status_to_evmc_status(exit_code), 0, 0, None)
        } else {
            ExecutionResult::failure()
        }
    }
}

#[inline]
#[allow(non_snake_case)]
fn evmc_revision_to_spec_id(revision: Revision) -> SpecId {
    use evmc_sys::evmc_revision::*;
    match revision {
        EVMC_FRONTIER => SpecId::FRONTIER,
        EVMC_HOMESTEAD => SpecId::HOMESTEAD,
        EVMC_TANGERINE_WHISTLE => SpecId::TANGERINE,
        EVMC_SPURIOUS_DRAGON => SpecId::SPURIOUS_DRAGON,
        EVMC_BYZANTIUM => SpecId::BYZANTIUM,
        EVMC_CONSTANTINOPLE => SpecId::CONSTANTINOPLE,
        EVMC_PETERSBURG => SpecId::PETERSBURG,
        EVMC_ISTANBUL => SpecId::ISTANBUL,
        EVMC_BERLIN => SpecId::BERLIN,
        EVMC_LONDON => SpecId::LONDON,
        EVMC_PARIS => SpecId::MERGE,
        EVMC_SHANGHAI => SpecId::SHANGHAI,
        EVMC_CANCUN => SpecId::CANCUN,
        EVMC_PRAGUE => SpecId::PRAGUE,
        EVMC_OSAKA => SpecId::OSAKA,
        EVMC_EXPERIMENTAL => SpecId::PRAGUE,
    }
}

#[inline]
fn evmc_address_to_address(address: &evmc_address) -> Address {
    unsafe { std::mem::transmute(address.bytes) }
}

#[inline]
fn status_to_evmc_status(status: ExitStatusCode) -> StatusCode {
    match status {
        ExitStatusCode::Continue
        | ExitStatusCode::Return
        | ExitStatusCode::Stop
        | ExitStatusCode::SelfDestruct => StatusCode::EVMC_SUCCESS,
        ExitStatusCode::Revert
        | ExitStatusCode::OutOfFunds
        | ExitStatusCode::CreateInitCodeStartingEF00
        | ExitStatusCode::InvalidEOFInitCode
        | ExitStatusCode::InvalidExtDelegatecallTarget => StatusCode::EVMC_REVERT,
        ExitStatusCode::OutOfGas | ExitStatusCode::MemoryOOG | ExitStatusCode::PrecompileOOG => {
            StatusCode::EVMC_OUT_OF_GAS
        }
        ExitStatusCode::CallTooDeep => StatusCode::EVMC_CALL_DEPTH_EXCEEDED,
        ExitStatusCode::InvalidOperandOOG => StatusCode::EVMC_ARGUMENT_OUT_OF_RANGE,
        ExitStatusCode::MemoryLimitOOG => StatusCode::EVMC_OUT_OF_MEMORY,
        ExitStatusCode::OutOfOffset => StatusCode::EVMC_INVALID_MEMORY_ACCESS,
        ExitStatusCode::NotActivated | ExitStatusCode::OpcodeNotFound => {
            StatusCode::EVMC_UNDEFINED_INSTRUCTION
        }
        ExitStatusCode::CallNotAllowedInsideStatic
        | ExitStatusCode::StateChangeDuringStaticCall => StatusCode::EVMC_STATIC_MODE_VIOLATION,
        ExitStatusCode::InvalidFEOpcode => StatusCode::EVMC_INVALID_INSTRUCTION,
        ExitStatusCode::InvalidJump => StatusCode::EVMC_BAD_JUMP_DESTINATION,
        ExitStatusCode::StackOverflow => StatusCode::EVMC_STACK_OVERFLOW,
        ExitStatusCode::StackUnderflow => StatusCode::EVMC_STACK_UNDERFLOW,
        ExitStatusCode::PrecompileError => StatusCode::EVMC_PRECOMPILE_FAILURE,
        ExitStatusCode::CreateCollision
        | ExitStatusCode::OverflowPayment
        | ExitStatusCode::NonceOverflow
        | ExitStatusCode::CreateContractSizeLimit
        | ExitStatusCode::CreateContractStartingWithEF
        | ExitStatusCode::CreateInitCodeSizeLimit
        | ExitStatusCode::ReturnContractInNotInitEOF
        | ExitStatusCode::EOFOpcodeDisabledInLegacy
        | ExitStatusCode::EOFFunctionStackOverflow
        | ExitStatusCode::EofAuxDataOverflow
        | ExitStatusCode::EofAuxDataTooSmall
        | ExitStatusCode::InvalidExtCallTarget
        | ExitStatusCode::FatalExternalError
        | ExitStatusCode::Suspend => StatusCode::EVMC_FAILURE,
    }
}

#[inline]
fn evmc_status_to_status(status: StatusCode) -> ExitStatusCode {
    match status {
        StatusCode::EVMC_SUCCESS => ExitStatusCode::Return,
        StatusCode::EVMC_REVERT => ExitStatusCode::Revert,
        StatusCode::EVMC_OUT_OF_GAS => ExitStatusCode::OutOfGas,
        StatusCode::EVMC_CALL_DEPTH_EXCEEDED => ExitStatusCode::CallTooDeep,
        StatusCode::EVMC_ARGUMENT_OUT_OF_RANGE => ExitStatusCode::InvalidOperandOOG,
        StatusCode::EVMC_OUT_OF_MEMORY => ExitStatusCode::MemoryLimitOOG,
        StatusCode::EVMC_INVALID_MEMORY_ACCESS => ExitStatusCode::OutOfOffset,
        StatusCode::EVMC_UNDEFINED_INSTRUCTION => ExitStatusCode::OpcodeNotFound,
        StatusCode::EVMC_STATIC_MODE_VIOLATION => ExitStatusCode::StateChangeDuringStaticCall,
        StatusCode::EVMC_INVALID_INSTRUCTION => ExitStatusCode::InvalidFEOpcode,
        StatusCode::EVMC_BAD_JUMP_DESTINATION => ExitStatusCode::InvalidJump,
        StatusCode::EVMC_STACK_OVERFLOW => ExitStatusCode::StackOverflow,
        StatusCode::EVMC_STACK_UNDERFLOW => ExitStatusCode::StackUnderflow,
        StatusCode::EVMC_PRECOMPILE_FAILURE => ExitStatusCode::PrecompileError,
        StatusCode::EVMC_CONTRACT_VALIDATION_FAILURE
        | StatusCode::EVMC_WASM_UNREACHABLE_INSTRUCTION
        | StatusCode::EVMC_WASM_TRAP
        | StatusCode::EVMC_INSUFFICIENT_BALANCE
        | StatusCode::EVMC_INTERNAL_ERROR
        | StatusCode::EVMC_REJECTED
        | StatusCode::EVMC_FAILURE => ExitStatusCode::FatalExternalError,
    }
}

#[inline]
fn call_kind_to_evmc_msg_kind(kind: CallKind) -> MessageKind {
    match kind {
        CallKind::EofCreate => MessageKind::EVMC_EOFCREATE,
        CallKind::Call => MessageKind::EVMC_CALL,
        CallKind::Callcode => MessageKind::EVMC_CALLCODE,
        CallKind::Delegatecall => MessageKind::EVMC_DELEGATECALL,
        CallKind::Staticcall => MessageKind::EVMC_CALL,
        CallKind::Create => MessageKind::EVMC_CREATE,
        CallKind::Create2 => MessageKind::EVMC_CREATE2,
        CallKind::ReturnContract
        | CallKind::ExtCall
        | CallKind::ExtStaticcall
        | CallKind::ExtDelegatecall => {
            unimplemented!("{:?}", kind)
        }
    }
}
