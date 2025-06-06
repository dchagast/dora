use crate::{ExitStatusCode, context::RuntimeContext};
use dora_primitives::{Address, B256, Bytes, U256};
use thiserror::Error;

/// The type of a `*CALL*` instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CallType {
    /// `CALL`.
    Call,
    /// `STATICCALL`.
    Staticcall,
    /// `DELEGATECALL`.
    Delegatecall,
    /// `CALLCODE`.
    Callcode,
}

/// The kind of a `EXT*CALL` instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ExtCallType {
    /// `EXTCALL`.
    Call,
    /// `EXTDELEGATECALL`.
    Delegatecall,
    /// `EXTSTATICCALL`.
    Staticcall,
}

#[derive(Error, Debug)]
#[error("Couldn't parse CallType from u8")]
pub struct CallTypeParseError;

impl TryFrom<u8> for CallType {
    type Error = CallTypeParseError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            x if x == CallType::Call as u8 => Ok(CallType::Call),
            x if x == CallType::Staticcall as u8 => Ok(CallType::Staticcall),
            x if x == CallType::Delegatecall as u8 => Ok(CallType::Delegatecall),
            x if x == CallType::Callcode as u8 => Ok(CallType::Callcode),
            _ => Err(CallTypeParseError),
        }
    }
}

impl TryFrom<u8> for ExtCallType {
    type Error = CallTypeParseError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            x if x == ExtCallType::Call as u8 => Ok(ExtCallType::Call),
            x if x == ExtCallType::Staticcall as u8 => Ok(ExtCallType::Staticcall),
            x if x == ExtCallType::Delegatecall as u8 => Ok(ExtCallType::Delegatecall),
            _ => Err(CallTypeParseError),
        }
    }
}

/// The kind of call-like instructions for the Host API.
#[derive(Debug, Clone)]
#[repr(u8)]
pub enum CallKind {
    Call = 0,
    Callcode = 1,
    Delegatecall = 2,
    Staticcall = 3,
    ExtCall = 4,
    ExtStaticcall = 5,
    ExtDelegatecall = 6,
    Create = 7,
    Create2 = 8,
    EofCreate = 9,
    ReturnContract = 10,
}

impl From<CallType> for CallKind {
    fn from(value: CallType) -> Self {
        match value {
            CallType::Call => CallKind::Call,
            CallType::Staticcall => CallKind::Staticcall,
            CallType::Delegatecall => CallKind::Delegatecall,
            CallType::Callcode => CallKind::Callcode,
        }
    }
}

impl From<ExtCallType> for CallKind {
    fn from(value: ExtCallType) -> Self {
        match value {
            ExtCallType::Call => CallKind::ExtCall,
            ExtCallType::Staticcall => CallKind::ExtStaticcall,
            ExtCallType::Delegatecall => CallKind::ExtDelegatecall,
        }
    }
}

/// The message describing a VM call, including a zero-depth calls from a transaction origin.
#[derive(Debug, Clone)]
pub struct CallMessage {
    /// The kind of of the execution.
    pub kind: CallKind,
    /// The call data of the execution.
    pub input: Bytes,
    /// The init code of the execution for the EOFCreate operation.
    pub init_code: Bytes,
    /// Call value.
    pub value: U256,
    /// The present depth of the message call stack.
    pub depth: u32,
    /// The gas limit of the execution.
    pub gas_limit: u64,
    /// This caller is invoking of the execution.
    pub caller: Address,
    /// Target address, this account storage is going to be modified.
    pub recipient: Address,
    /// The optional value used in new contract address construction.
    pub salt: Option<B256>,
    /// The address of the code to be executed.
    pub code_address: Address,
    /// Whether the call is a static call, or is initiated inside a static call.
    pub is_static: bool,
    /// Whether the call is initiated from EOF bytecode.
    pub is_eof_init: bool,
    /// Whether the validate EOF bytecode
    pub validate_eof: bool,
}

impl CallMessage {
    /// Returns `true` if the call will transfer a non-zero value.
    #[inline]
    pub fn transfers_value(&self) -> bool {
        self.value > U256::ZERO
    }
}

/// The VM call result.
#[derive(Debug, Clone)]
pub struct CallResult {
    /// The execution status code.
    pub status: ExitStatusCode,
    /// The gas limit of the execution.
    pub gas_limit: u64,
    /// The amount of gas left after the execution.
    pub gas_remaining: u64,
    /// The refunded gas accumulated from this execution and its sub-calls.
    pub gas_refunded: i64,
    /// The output contains data coming from RETURN opcode.
    pub output: Bytes,
    // An optional address associated with the create and create2 kinds.
    pub create_address: Option<Address>,
}

impl Default for CallResult {
    fn default() -> Self {
        Self::new_with_gas_limit(0)
    }
}

impl CallResult {
    /// New a default call result with the gas_limit.
    #[inline]
    pub fn new_with_gas_limit(gas_limit: u64) -> Self {
        Self::new_with_gas_limit_and_status(gas_limit, ExitStatusCode::Stop)
    }

    /// New a default call result with the gas_limit and status.
    #[inline]
    pub fn new_with_gas_limit_and_status(gas_limit: u64, status: ExitStatusCode) -> Self {
        Self {
            status,
            gas_limit,
            gas_remaining: gas_limit,
            gas_refunded: Default::default(),
            output: Default::default(),
            create_address: Default::default(),
        }
    }

    /// New a call result with the runtime context and gas limit
    #[inline]
    pub fn new_with_runtime_context(runtime_context: &RuntimeContext) -> Self {
        Self {
            gas_limit: runtime_context.gas_limit(),
            status: runtime_context.status(),
            gas_remaining: runtime_context.gas_remaining(),
            gas_refunded: runtime_context.gas_refunded(),
            output: runtime_context.return_bytes(),
            create_address: None,
        }
    }

    /// Records an explicit cost.
    ///
    /// Returns `false` if the gas limit is exceeded.
    #[inline]
    pub fn record_cost(&mut self, cost: u64) -> bool {
        let (remaining, overflow) = self.gas_remaining.overflowing_sub(cost);
        let success = !overflow;
        if success {
            self.gas_remaining = remaining;
        }
        success
    }

    /// Records a refund value.
    #[inline]
    pub fn record_refund(&mut self, refund: i64) {
        self.gas_refunded += refund;
    }

    /// Set a refund value for final refund.
    ///
    /// Max refund value is limited to Nth part (depending of fork) of gas cost.
    ///
    /// Related to EIP-3529: Reduction in refunds
    #[inline]
    pub fn set_final_refund(&mut self, is_london: bool) {
        let max_refund_quotient = if is_london { 5 } else { 2 };
        self.gas_refunded =
            (self.gas_refunded as u64).min(self.gas_used() / max_refund_quotient) as i64;
    }

    /// Set a refund value. This overrides the current refund value.
    #[inline]
    pub fn set_refund(&mut self, refund: i64) {
        self.gas_refunded = refund;
    }

    /// Set a spent value. This overrides the current spent value.
    #[inline]
    pub fn set_spent(&mut self, spent: u64) {
        self.gas_remaining = self.gas_limit.saturating_sub(spent);
    }

    /// Returns the total amount of gas used.
    #[inline]
    pub const fn gas_used(&self) -> u64 {
        self.gas_limit - self.gas_remaining
    }

    /// Returns the total amount of gas spent, minus the refunded gas.
    #[inline]
    pub const fn spent_sub_refunded(&self) -> u64 {
        self.gas_used().saturating_sub(self.gas_refunded as u64)
    }
}
