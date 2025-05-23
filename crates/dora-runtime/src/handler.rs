use std::sync::Arc;

use crate::{
    call::CallResult,
    context::{Contract, VMContext},
    db::Database,
    result::VMError,
};

#[derive(Debug)]
pub struct Frame {
    /// Contract infortmation.
    pub contract: Contract,
    /// The gas limit
    pub gas_limit: u64,
    /// Depth in the call stack.
    pub depth: usize,
    /// Whether the call is a static call, or is initiated inside a static call.
    pub is_static: bool,
    /// Whether the call is initiated from EOF bytecode.
    pub is_eof_init: bool,
    /// Whether validate EOF bytecode.
    pub validate_eof: bool,
}

pub type CallFrameHandle<DB> =
    Arc<dyn Fn(Frame, &mut VMContext<DB>) -> Result<CallResult, VMError>>;

/// Handler acts as a proxy and allow to define different behavior for different
/// sections of the code.
pub struct Handler<DB: Database> {
    /// Call frame handler.
    pub call_handler: CallFrameHandle<DB>,
}
