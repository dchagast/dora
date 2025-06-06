use crate::backend::IntCC;
use crate::dora::gas::compute_copy_cost;
use crate::{
    block_argument,
    conversion::rewriter::Rewriter,
    dora::{conversion::ConversionPass, memory},
    errors::Result,
    operands, rewrite_ctx,
};
use crate::{gas_or_fail, if_here, u256_as_usize_or_fail};
use dora_runtime::{ExitStatusCode, symbols};
use melior::dialect::func;
use melior::ir::attribute::FlatSymbolRefAttribute;
use melior::{
    Context,
    dialect::{
        arith,
        llvm::{self, LoadStoreOptions},
        ods,
    },
    ir::{Block, attribute::IntegerAttribute, operation::OperationRef, r#type::IntegerType},
};

impl ConversionPass<'_> {
    pub(crate) fn mload(context: &Context, op: &OperationRef<'_, '_>) -> Result<()> {
        operands!(op, offset);
        block_argument!(op, syscall_ctx, gas_counter_ptr);
        let rewriter = Rewriter::new_with_op(context, *op);
        let uint8 = IntegerType::new(context, 8);
        let uint256 = rewriter.intrinsics.i256_ty;
        let value_size = rewriter.make(rewriter.iconst_64(32))?;
        let size_is_not_zero =
            rewriter.make(rewriter.icmp_imm(IntCC::NotEqual, value_size, 0)?)?;
        if_here!(op, rewriter, size_is_not_zero, {
            u256_as_usize_or_fail!(op, rewriter, offset);
            memory::resize_memory(
                context,
                op,
                &rewriter,
                syscall_ctx,
                gas_counter_ptr,
                offset,
                value_size,
            )?;
        });
        rewrite_ctx!(context, op, rewriter, location);
        let offset = rewriter.make(arith::trunci(offset, rewriter.intrinsics.i64_ty, location))?;
        let memory_ptr = rewriter.make(func::call(
            context,
            FlatSymbolRefAttribute::new(context, symbols::MEMORY_PTR),
            &[syscall_ctx.into()],
            &[rewriter.ptr_ty()],
            location,
        ))?;
        let memory_destination = rewriter.make(llvm::get_element_ptr_dynamic(
            context,
            memory_ptr,
            &[offset],
            uint8.into(),
            rewriter.ptr_ty(),
            location,
        ))?;
        let read_value = rewriter.make(llvm::load(
            context,
            memory_destination,
            uint256,
            location,
            LoadStoreOptions::new()
                .align(IntegerAttribute::new(IntegerType::new(context, 64).into(), 1).into()),
        ))?;

        // Check system endianness before storing the value
        if cfg!(target_endian = "little") {
            // Convert the value to big-endian if the system is little-endian
            rewriter.make(llvm::intr_bswap(read_value, uint256, location))?;
        }
        Ok(())
    }

    pub(crate) fn mstore(
        context: &Context,
        op: &OperationRef<'_, '_>,
        byte_size: u32, // Pass `32` for `mstore` and `1` for `mstore8`
    ) -> Result<()> {
        operands!(op, offset, value);
        block_argument!(op, syscall_ctx, gas_counter_ptr);
        let rewriter = Rewriter::new_with_op(context, *op);
        let location = rewriter.get_insert_location();
        let uint8 = rewriter.intrinsics.i8_ty;

        // If byte_size is 1 (mstore8), truncate value to 1 byte
        let value = if byte_size == 1 {
            rewriter.make(arith::trunci(value, rewriter.intrinsics.i8_ty, location))?
        } else {
            value
        };

        // Calculate value size (1 byte for mstore8, 32 bytes for mstore)
        let value_size = rewriter.make(rewriter.iconst_64(byte_size as i64))?;
        let size_is_not_zero =
            rewriter.make(rewriter.icmp_imm(IntCC::NotEqual, value_size, 0)?)?;
        if_here!(op, rewriter, size_is_not_zero, {
            u256_as_usize_or_fail!(op, rewriter, offset);
            memory::resize_memory(
                context,
                op,
                &rewriter,
                syscall_ctx,
                gas_counter_ptr,
                offset,
                value_size,
            )?;
        });
        rewrite_ctx!(context, op, rewriter, location);
        let offset = rewriter.make(arith::trunci(offset, rewriter.intrinsics.i64_ty, location))?;
        let memory_ptr = rewriter.make(func::call(
            context,
            FlatSymbolRefAttribute::new(context, symbols::MEMORY_PTR),
            &[syscall_ctx.into()],
            &[rewriter.ptr_ty()],
            location,
        ))?;
        // Memory_destination = memory_ptr + offset
        let memory_destination = rewriter.make(llvm::get_element_ptr_dynamic(
            context,
            memory_ptr,
            &[offset],
            uint8,
            rewriter.ptr_ty(),
            location,
        ))?;

        // Check system endianness before storing the value
        let value = if byte_size > 1 && cfg!(target_endian = "little") {
            // Convert the value to big-endian if the system is little-endian
            rewriter.make(llvm::intr_bswap(
                value,
                rewriter.intrinsics.i256_ty,
                location,
            ))?
        } else {
            value
        };
        rewriter.create(llvm::store(
            context,
            value,
            memory_destination,
            location,
            LoadStoreOptions::new()
                .align(IntegerAttribute::new(IntegerType::new(context, 64).into(), 1).into()),
        ));
        Ok(())
    }

    pub(crate) fn msize(context: &Context, op: &OperationRef<'_, '_>) -> Result<()> {
        block_argument!(op, syscall_ctx);
        rewrite_ctx!(context, op, rewriter, location);

        let uint256 = rewriter.intrinsics.i256_ty;
        let memory_size = rewriter.make(func::call(
            context,
            FlatSymbolRefAttribute::new(context, symbols::MEMORY_SIZE),
            &[syscall_ctx.into()],
            &[rewriter.intrinsics.i64_ty],
            location,
        ))?;
        rewriter.make(arith::extui(memory_size, uint256, location))?;
        Ok(())
    }

    pub(crate) fn mcopy(context: &Context, op: &OperationRef<'_, '_>) -> Result<()> {
        operands!(op, dest_offset, src_offset, size);
        block_argument!(op, syscall_ctx, gas_counter_ptr);
        let rewriter = Rewriter::new_with_op(context, *op);
        let location = rewriter.get_insert_location();
        let uint8 = rewriter.intrinsics.i8_ty;
        u256_as_usize_or_fail!(op, rewriter, size);
        let gas = compute_copy_cost(&rewriter, size)?;
        gas_or_fail!(op, rewriter, gas, gas_counter_ptr);
        let rewriter = Rewriter::new_with_op(context, *op);
        let size_is_not_zero = rewriter.make(rewriter.icmp_imm(IntCC::NotEqual, size, 0)?)?;
        if_here!(op, rewriter, size_is_not_zero, {
            u256_as_usize_or_fail!(op, rewriter, dest_offset);
            u256_as_usize_or_fail!(op, rewriter, src_offset);
            let offset = rewriter.make(arith::maxui(dest_offset, src_offset, location))?;
            memory::resize_memory(
                context,
                op,
                &rewriter,
                syscall_ctx,
                gas_counter_ptr,
                offset,
                size,
            )?;
        });
        rewrite_ctx!(context, op, rewriter, location);
        let memory_ptr = rewriter.make(func::call(
            context,
            FlatSymbolRefAttribute::new(context, symbols::MEMORY_PTR),
            &[syscall_ctx.into()],
            &[rewriter.ptr_ty()],
            location,
        ))?;
        // memory_destination = memory_ptr + offset
        let source = rewriter.make(llvm::get_element_ptr_dynamic(
            context,
            memory_ptr,
            &[src_offset],
            uint8,
            rewriter.ptr_ty(),
            location,
        ))?;
        // memory_destination = memory_ptr + dest_offset
        let destination = rewriter.make(llvm::get_element_ptr_dynamic(
            context,
            memory_ptr,
            &[dest_offset],
            uint8,
            rewriter.ptr_ty(),
            location,
        ))?;
        // Perform memory move
        rewriter.create(
            ods::llvm::intr_memmove(
                context,
                destination,
                source,
                size,
                IntegerAttribute::new(IntegerType::new(context, 1).into(), 0),
                location,
            )
            .into(),
        );
        Ok(())
    }
}
