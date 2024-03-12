use std::collections::HashMap;
use std::rc::Rc;

use crate::runtime::{Register, Runtime};
use crate::syscall::precompiles::blake3::Blake3CompressInnerChip;
use crate::syscall::precompiles::edwards::EdAddAssignChip;
use crate::syscall::precompiles::edwards::EdDecompressChip;
use crate::syscall::precompiles::k256::K256DecompressChip;
use crate::syscall::precompiles::keccak256::KeccakPermuteChip;
use crate::syscall::precompiles::sha256::{ShaCompressChip, ShaExtendChip};
use crate::syscall::precompiles::weierstrass::WeierstrassAddAssignChip;
use crate::syscall::precompiles::weierstrass::WeierstrassDoubleAssignChip;
use crate::syscall::{
    SyscallEnterUnconstrained, SyscallExitUnconstrained, SyscallHalt, SyscallLWA, SyscallWrite,
};
use crate::utils::ec::edwards::ed25519::{Ed25519, Ed25519Parameters};
use crate::utils::ec::weierstrass::secp256k1::Secp256k1;
use crate::{cpu::MemoryReadRecord, cpu::MemoryWriteRecord, runtime::ExecutionRecord};

/// A system call is invoked by the the `ecall` instruction with a specific value in register t0.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[allow(non_camel_case_types)]
pub enum SyscallCode {
    /// Halts the program.
    HALT = 0x01_00_00_00,

    /// Loads a word supplied from the prover.
    LWA = 0x00_00_00_01,

    /// Write to the output buffer.
    WRITE = 0x00_00_00_02,

    /// Enter unconstrained block.
    ENTER_UNCONSTRAINED = 0x00_00_00_03,

    /// Exit unconstrained block.
    EXIT_UNCONSTRAINED = 0x00_00_00_04,

    /// Executes the `SHA_EXTEND` precompile.
    SHA_EXTEND = 0x00_80_01_00,

    /// Executes the `SHA_COMPRESS` precompile.
    SHA_COMPRESS = 0x00_80_01_01,

    /// Executes the `ED_ADD` precompile.
    ED_ADD = 0x00_80_01_02,

    /// Executes the `ED_DECOMPRESS` precompile.
    ED_DECOMPRESS = 0x00_80_01_03,

    /// Executes the `KECCAK_PERMUTE` precompile.
    KECCAK_PERMUTE = 0x00_80_01_04,

    /// Executes the `SECP256K1_ADD` precompile.
    SECP256K1_ADD = 0x00_80_01_05,

    /// Executes the `SECP256K1_DOUBLE` precompile.
    SECP256K1_DOUBLE = 0x00_80_01_06,

    /// Executes the `SECP256K1_DECOMPRESS` precompile.
    SECP256K1_DECOMPRESS = 0x00_80_01_07,

    /// Executes the `BLAKE3_COMPRESS_INNER` precompile.
    BLAKE3_COMPRESS_INNER = 0x00_80_01_08,
}

impl SyscallCode {
    /// Create a syscall from a u32.
    pub fn from_u32(value: u32) -> Self {
        match value {
            0x01_00_00_00 => SyscallCode::HALT,
            0x00_00_00_01 => SyscallCode::LWA,
            0x00_00_00_02 => SyscallCode::WRITE,
            0x00_00_00_03 => SyscallCode::ENTER_UNCONSTRAINED,
            0x00_00_00_04 => SyscallCode::EXIT_UNCONSTRAINED,
            0x00_80_01_00 => SyscallCode::SHA_EXTEND,
            0x00_80_01_01 => SyscallCode::SHA_COMPRESS,
            0x00_80_01_02 => SyscallCode::ED_ADD,
            0x00_80_01_03 => SyscallCode::ED_DECOMPRESS,
            0x00_80_01_04 => SyscallCode::KECCAK_PERMUTE,
            0x00_80_01_05 => SyscallCode::SECP256K1_ADD,
            0x00_80_01_06 => SyscallCode::SECP256K1_DOUBLE,
            0x00_80_01_07 => SyscallCode::SECP256K1_DECOMPRESS,
            0x00_80_01_08 => SyscallCode::BLAKE3_COMPRESS_INNER,
            _ => panic!("invalid syscall number: {}", value),
        }
    }
}

pub trait Syscall {
    /// Execute the syscall and return the resulting value of register a0. `arg1` and `arg2` are the
    /// values in registers X10 and X11, respectively. While not a hard requirement, the convention
    /// is that the return value is only for system calls such as `LWA`. Precompiles use `arg1` and
    /// `arg2` to denote the addresses of the input data, and write the result to the memory at
    /// `arg1`.
    fn execute(&self, ctx: &mut SyscallContext, arg1: u32, arg2: u32) -> Option<u32>;

    /// The number of extra cycles that the syscall takes to execute. Unless this syscall is complex
    /// and requires many cycles, this should be zero.
    fn num_extra_cycles(&self) -> u32 {
        0
    }
}

/// A runtime for syscalls that is protected so that developers cannot arbitrarily modify the runtime.
pub struct SyscallContext<'a> {
    current_shard: u32,
    pub clk: u32,

    pub(crate) next_pc: u32,
    pub(crate) rt: &'a mut Runtime,
}

impl<'a> SyscallContext<'a> {
    pub fn new(runtime: &'a mut Runtime) -> Self {
        let current_shard = runtime.current_shard();
        let clk = runtime.state.clk;
        Self {
            current_shard,
            clk,
            next_pc: runtime.state.pc.wrapping_add(4),
            rt: runtime,
        }
    }

    pub fn record_mut(&mut self) -> &mut ExecutionRecord {
        &mut self.rt.record
    }

    pub fn current_shard(&self) -> u32 {
        self.rt.state.current_shard
    }

    pub fn mr(&mut self, addr: u32) -> (MemoryReadRecord, u32) {
        let record = self.rt.mr(addr, self.current_shard, self.clk);
        (record, record.value)
    }

    pub fn mr_slice(&mut self, addr: u32, len: usize) -> (Vec<MemoryReadRecord>, Vec<u32>) {
        let mut records = Vec::new();
        let mut values = Vec::new();
        for i in 0..len {
            let (record, value) = self.mr(addr + i as u32 * 4);
            records.push(record);
            values.push(value);
        }
        (records, values)
    }

    pub fn mw(&mut self, addr: u32, value: u32) -> MemoryWriteRecord {
        self.rt.mw(addr, value, self.current_shard, self.clk)
    }

    pub fn mw_slice(&mut self, addr: u32, values: &[u32]) -> Vec<MemoryWriteRecord> {
        let mut records = Vec::new();
        for i in 0..values.len() {
            let record = self.mw(addr + i as u32 * 4, values[i]);
            records.push(record);
        }
        records
    }

    /// Get the current value of a register, but doesn't use a memory record.
    /// This is generally unconstrained, so you must be careful using it.
    pub fn register_unsafe(&self, register: Register) -> u32 {
        self.rt.register(register)
    }

    pub fn byte_unsafe(&self, addr: u32) -> u8 {
        self.rt.byte(addr)
    }

    pub fn word_unsafe(&self, addr: u32) -> u32 {
        self.rt.word(addr)
    }

    pub fn slice_unsafe(&self, addr: u32, len: usize) -> Vec<u32> {
        let mut values = Vec::new();
        for i in 0..len {
            values.push(self.rt.word(addr + i as u32 * 4));
        }
        values
    }

    pub fn set_next_pc(&mut self, next_pc: u32) {
        self.next_pc = next_pc;
    }
}

pub fn default_syscall_map() -> HashMap<SyscallCode, Rc<dyn Syscall>> {
    let mut syscall_map = HashMap::<SyscallCode, Rc<dyn Syscall>>::default();
    syscall_map.insert(SyscallCode::HALT, Rc::new(SyscallHalt {}));
    syscall_map.insert(SyscallCode::LWA, Rc::new(SyscallLWA::new()));
    syscall_map.insert(SyscallCode::SHA_EXTEND, Rc::new(ShaExtendChip::new()));
    syscall_map.insert(SyscallCode::SHA_COMPRESS, Rc::new(ShaCompressChip::new()));
    syscall_map.insert(
        SyscallCode::ED_ADD,
        Rc::new(EdAddAssignChip::<Ed25519>::new()),
    );
    syscall_map.insert(
        SyscallCode::ED_DECOMPRESS,
        Rc::new(EdDecompressChip::<Ed25519Parameters>::new()),
    );
    syscall_map.insert(
        SyscallCode::KECCAK_PERMUTE,
        Rc::new(KeccakPermuteChip::new()),
    );
    syscall_map.insert(
        SyscallCode::SECP256K1_ADD,
        Rc::new(WeierstrassAddAssignChip::<Secp256k1>::new()),
    );
    syscall_map.insert(
        SyscallCode::SECP256K1_DOUBLE,
        Rc::new(WeierstrassDoubleAssignChip::<Secp256k1>::new()),
    );
    syscall_map.insert(SyscallCode::SHA_COMPRESS, Rc::new(ShaCompressChip::new()));
    syscall_map.insert(
        SyscallCode::SECP256K1_DECOMPRESS,
        Rc::new(K256DecompressChip::new()),
    );
    syscall_map.insert(
        SyscallCode::BLAKE3_COMPRESS_INNER,
        Rc::new(Blake3CompressInnerChip::new()),
    );
    syscall_map.insert(
        SyscallCode::ENTER_UNCONSTRAINED,
        Rc::new(SyscallEnterUnconstrained::new()),
    );
    syscall_map.insert(
        SyscallCode::EXIT_UNCONSTRAINED,
        Rc::new(SyscallExitUnconstrained::new()),
    );
    syscall_map.insert(SyscallCode::WRITE, Rc::new(SyscallWrite::new()));

    syscall_map
}

#[cfg(test)]
mod tests {
    use super::default_syscall_map;

    #[test]
    fn test_syscall_num_cycles_encoding() {
        for (syscall_code, syscall_impl) in default_syscall_map().iter() {
            let syscall_id = syscall_code.clone() as u32;
            let encoded_num_cycles = syscall_id.to_le_bytes()[2];
            assert_eq!(syscall_impl.num_extra_cycles(), encoded_num_cycles as u32);
        }
    }
}
