//! Reference: [revm](https://github.com/bluealloy/revm)
use std::{collections::hash_map::Entry, mem};

use crate::{
    ExitStatusCode,
    db::{Database, StorageSlot},
    host::{AccountLoad, SStoreResult, SStoreSlot, SelfDestructResult},
};
pub use dora_primitives::EvmState as State;
use dora_primitives::{
    Account, Address, B256, Bytecode, Bytes32, EVMBytecode, EmptyBytecode, KECCAK_EMPTY, Log,
    SpecId, StateLoad, U256,
};
use rustc_hash::{FxHashMap, FxHashSet};

pub type TransientStorage = FxHashMap<(Address, Bytes32), Bytes32>;

/// A journal of state changes internal to the VM.
#[derive(Debug, Clone)]
pub struct JournaledState {
    /// The current state.
    pub state: State,
    /// Transient storage that is discarded after every transaction.
    pub transient_storage: TransientStorage,
    /// Logs captures all log entries emitted by the contract, which can be used for event tracking
    /// and off-chain data analysis. Logs are essential for notifying external observers about
    /// significant events that occurred during contract execution.
    pub logs: Vec<Log>,
    /// Depth in the call stack.
    pub depth: usize,
    /// The journal of state changes, one for each call.
    pub journal: Vec<Vec<JournalEntry>>,
    /// VM spec id
    pub spec_id: SpecId,
    /// Warm loaded addresses are used to check if loaded address
    /// should be considered cold or warm loaded when the account
    /// is first accessed.
    pub warm_preloaded_addresses: FxHashSet<Address>,
}

impl JournaledState {
    /// Create new JournaledState.
    ///
    /// warm_preloaded_addresses is used to determine if address is considered warm loaded.
    /// In ordinary case this is precompile or beneficiary.
    ///
    /// # Note
    ///
    /// This function will journal state after Spurious Dragon fork.
    /// And will not take into account if account is not existing or empty.
    pub fn new(spec_id: SpecId, warm_preloaded_addresses: FxHashSet<Address>) -> JournaledState {
        Self {
            state: Default::default(),
            transient_storage: Default::default(),
            logs: Vec::new(),
            journal: vec![vec![]],
            depth: 0,
            spec_id,
            warm_preloaded_addresses,
        }
    }

    /// Return reference to state.
    #[inline]
    pub fn state(&mut self) -> &mut State {
        &mut self.state
    }

    /// Sets SpecId.
    #[inline]
    pub fn set_spec_id(&mut self, spec_id: SpecId) {
        self.spec_id = spec_id;
    }

    /// Mark account as touched as only touched accounts will be added to state.
    /// This is especially important for state clear where touched empty accounts needs to
    /// be removed from state.
    #[inline]
    pub fn touch(&mut self, address: &Address) {
        if let Some(account) = self.state.get_mut(address) {
            Self::touch_account(self.journal.last_mut().unwrap(), address, account);
        }
    }

    /// Mark account as touched.
    #[inline]
    fn touch_account(journal: &mut Vec<JournalEntry>, address: &Address, account: &mut Account) {
        if !account.is_touched() {
            journal.push(JournalEntry::AccountTouched { address: *address });
            account.mark_touch();
        }
    }

    /// Clears the JournaledState. Preserving only the spec.
    #[inline]
    pub fn clear(&mut self) {
        *self = Self::new(self.spec_id, Default::default());
    }

    /// Does cleanup and returns modified state.
    ///
    /// This resets the [JournaledState] to its initial state in [Self::new]
    #[inline]
    pub fn finalize(&mut self) -> (State, Vec<Log>) {
        let Self {
            state,
            transient_storage,
            logs,
            depth,
            journal,
            // kept, see [Self::new]
            spec_id: _,
            warm_preloaded_addresses: _,
        } = self;

        *transient_storage = TransientStorage::default();
        *journal = vec![vec![]];
        *depth = 0;
        let state = mem::take(state);
        let logs = mem::take(logs);

        (state, logs)
    }

    /// Returns the _loaded_ [Account] for the given address.
    ///
    /// This assumes that the account has already been loaded.
    ///
    /// # Panics
    ///
    /// Panics if the account has not been loaded and is missing from the state set.
    #[inline]
    pub fn account(&self, address: Address) -> &Account {
        self.state
            .get(&address)
            .expect("Account expected to be loaded") // Always assume that acc is already loaded
    }

    /// Returns call depth.
    #[inline]
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Set code and its hash to the account.
    ///
    /// Note: Assume account is warm and that hash is calculated from code.
    pub fn set_code_with_hash(&mut self, address: Address, code: Bytecode, hash: B256) {
        let account = self.state.get_mut(&address).unwrap();
        Self::touch_account(self.journal.last_mut().unwrap(), &address, account);

        self.journal
            .last_mut()
            .unwrap()
            .push(JournalEntry::CodeChange { address });

        account.info.code_hash = hash;
        account.info.code = Some(code);
    }

    /// use it only if you know that acc is warm
    /// Assume account is warm
    #[inline]
    pub fn set_code(&mut self, address: Address, code: Bytecode) {
        let hash = code.hash_slow();
        self.set_code_with_hash(address, code, hash);
    }

    pub fn inc_nonce(&mut self, address: Address) -> Option<u64> {
        let account = self.state.get_mut(&address).unwrap();
        // Check if nonce is going to overflow.
        if account.info.nonce == u64::MAX {
            return None;
        }
        Self::touch_account(self.journal.last_mut().unwrap(), &address, account);
        self.journal
            .last_mut()
            .unwrap()
            .push(JournalEntry::NonceChange { address });

        account.info.nonce += 1;

        Some(account.info.nonce)
    }

    /// Transfers balance from two accounts. Returns error if sender balance is not enough.
    pub fn transfer<DB: Database>(
        &mut self,
        from: &Address,
        to: &Address,
        balance: U256,
        db: &mut DB,
    ) -> Result<Option<ExitStatusCode>, DB::Error> {
        // load accounts
        self.load_account(*from, db)?;
        self.load_account(*to, db)?;

        // sub balance from
        let from_account = &mut self.state.get_mut(from).unwrap();
        Self::touch_account(self.journal.last_mut().unwrap(), from, from_account);
        let from_balance = &mut from_account.info.balance;
        let Some(from_balance_incr) = from_balance.checked_sub(balance) else {
            return Ok(Some(ExitStatusCode::OutOfFunds));
        };
        *from_balance = from_balance_incr;

        // add balance to
        let to_account = &mut self.state.get_mut(to).unwrap();
        Self::touch_account(self.journal.last_mut().unwrap(), to, to_account);
        let to_balance = &mut to_account.info.balance;
        let Some(to_balance_decr) = to_balance.checked_add(balance) else {
            return Ok(Some(ExitStatusCode::OverflowPayment));
        };
        *to_balance = to_balance_decr;
        // Overflow of U256 balance is not possible to happen on mainnet. We don't bother to return funds from from_acc.

        self.journal
            .last_mut()
            .unwrap()
            .push(JournalEntry::BalanceTransfer {
                from: *from,
                to: *to,
                balance,
            });

        Ok(None)
    }

    /// Create account or return false if collision is detected.
    ///
    /// There are few steps done:
    /// 1. Make created account warm loaded (AccessList) and this should
    ///     be done before subroutine checkpoint is created.
    /// 2. Check if there is collision of newly created account with existing one.
    /// 3. Mark created account as created.
    /// 4. Add fund to created account
    /// 5. Increment nonce of created account if SpuriousDragon is active
    /// 6. Decrease balance of caller account.
    ///
    /// # Panics
    ///
    /// Panics if the caller is not loaded inside of the VM state.
    /// This is should have been done inside `create_inner`.
    pub fn create_account_checkpoint(
        &mut self,
        caller: Address,
        address: Address,
        balance: U256,
        spec_id: SpecId,
    ) -> Result<JournalCheckpoint, ExitStatusCode> {
        // Enter subroutine
        let checkpoint = self.checkpoint();

        // Newly created account is present, as we just loaded it.
        let account = self.state.get_mut(&address).unwrap();
        let last_journal = self.journal.last_mut().unwrap();

        // New account can be created if:
        // Bytecode is not empty.
        // Nonce is not zero
        // Account is not precompile.
        if account.info.code_hash != KECCAK_EMPTY || account.info.nonce != 0 {
            self.checkpoint_revert(checkpoint);
            return Err(ExitStatusCode::CreateCollision);
        }

        // set account status to created.
        account.mark_created();

        // this entry will revert set nonce.
        last_journal.push(JournalEntry::AccountCreated { address });
        account.info.code = None;

        // touch account. This is important as for pre SpuriousDragon account could be
        // saved even empty.
        Self::touch_account(last_journal, &address, account);

        // Add balance to created account, as we already have target here.
        let Some(new_balance) = account.info.balance.checked_add(balance) else {
            self.checkpoint_revert(checkpoint);
            return Err(ExitStatusCode::OverflowPayment);
        };
        account.info.balance = new_balance;

        // EIP-161: State trie clearing (invariant-preserving alternative)
        if spec_id.is_enabled_in(SpecId::SPURIOUS_DRAGON) {
            // nonce is going to be reset to zero in AccountCreated journal entry.
            account.info.nonce = 1;
        }

        // Sub balance from caller
        let caller_account = self.state.get_mut(&caller).unwrap();
        // Balance is already checked in `create_inner`, so it is safe to just subtract.
        caller_account.info.balance = caller_account.info.balance.saturating_sub(balance);

        // add journal entry of transferred balance
        last_journal.push(JournalEntry::BalanceTransfer {
            from: caller,
            to: address,
            balance,
        });

        Ok(checkpoint)
    }

    /// Revert all changes that happened in given journal entries.
    fn journal_revert(
        state: &mut State,
        transient_storage: &mut TransientStorage,
        journal_entries: Vec<JournalEntry>,
        is_spurious_dragon_enabled: bool,
    ) {
        for entry in journal_entries.into_iter().rev() {
            match entry {
                JournalEntry::AccountWarmed { address } => {
                    state.get_mut(&address).unwrap().mark_cold();
                }
                JournalEntry::AccountTouched { address } => {
                    let precompiles3_addr = Address::from_slice(&[
                        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3,
                    ]);
                    if is_spurious_dragon_enabled && address == precompiles3_addr {
                        continue;
                    }
                    // remove touched status
                    state.get_mut(&address).unwrap().unmark_touch();
                }
                JournalEntry::AccountDestroyed {
                    address,
                    target,
                    was_destroyed,
                    had_balance,
                } => {
                    let account = state.get_mut(&address).unwrap();
                    // set previous state of selfdestructed flag, as there could be multiple
                    // selfdestructs in one transaction.
                    if was_destroyed {
                        // flag is still selfdestructed
                        account.mark_selfdestruct();
                    } else {
                        // flag that is not selfdestructed
                        account.unmark_selfdestruct();
                    }
                    let had_balance = had_balance.to_u256();
                    account.info.balance = account.info.balance.saturating_add(had_balance);

                    if address != target {
                        let target = state.get_mut(&target).unwrap();
                        target.info.balance = target.info.balance.saturating_sub(had_balance);
                    }
                }
                JournalEntry::BalanceTransfer { from, to, balance } => {
                    // we don't need to check overflow and underflow when adding and subtracting the balance.
                    let from = state.get_mut(&from).unwrap();
                    from.info.balance = from.info.balance.saturating_add(balance);
                    let to = state.get_mut(&to).unwrap();
                    to.info.balance = to.info.balance.saturating_sub(balance);
                }
                JournalEntry::NonceChange { address } => {
                    state.get_mut(&address).unwrap().info.nonce -= 1;
                }
                JournalEntry::AccountCreated { address } => {
                    let account = &mut state.get_mut(&address).unwrap();
                    account.unmark_created();
                    account
                        .storage
                        .values_mut()
                        .for_each(|slot| slot.mark_cold());
                    account.info.nonce = 0;
                }
                JournalEntry::StorageWarmed { address, key } => {
                    state
                        .get_mut(&address)
                        .unwrap()
                        .storage
                        .get_mut(key.as_u256())
                        .unwrap()
                        .mark_cold();
                }
                JournalEntry::StorageChanged {
                    address,
                    key,
                    had_value,
                } => {
                    state
                        .get_mut(&address)
                        .unwrap()
                        .storage
                        .get_mut(key.as_u256())
                        .unwrap()
                        .present_value = had_value.to_u256();
                }
                JournalEntry::TransientStorageChange {
                    address,
                    key,
                    had_value,
                } => {
                    let tkey = (address, key);
                    if had_value.as_u256().is_zero() {
                        // if previous value is zero, remove it
                        transient_storage.remove(&tkey);
                    } else {
                        // if not zero, reinsert old value to transient storage.
                        transient_storage.insert(tkey, had_value);
                    }
                }
                JournalEntry::CodeChange { address } => {
                    let acc = state.get_mut(&address).unwrap();
                    acc.info.code_hash = KECCAK_EMPTY;
                    acc.info.code = None;
                }
            }
        }
    }

    /// Makes a checkpoint that in case of Revert can bring back state to this point.
    pub fn checkpoint(&mut self) -> JournalCheckpoint {
        let checkpoint = JournalCheckpoint {
            log_i: self.logs.len(),
            journal_i: self.journal.len(),
        };
        self.depth += 1;
        self.journal.push(Default::default());
        checkpoint
    }

    /// Commit the checkpoint.
    #[inline]
    pub fn checkpoint_commit(&mut self) {
        self.depth -= 1;
    }

    /// Reverts all changes to state until given checkpoint.
    pub fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint) {
        let is_spurious_dragon_enabled = self.spec_id.is_enabled_in(SpecId::SPURIOUS_DRAGON);
        let state = &mut self.state;
        let transient_storage = &mut self.transient_storage;
        self.depth -= 1;
        // iterate over last N journals sets and revert our global state
        let leng = self.journal.len();
        self.journal
            .iter_mut()
            .rev()
            .take(leng - checkpoint.journal_i)
            .for_each(|cs| {
                Self::journal_revert(
                    state,
                    transient_storage,
                    mem::take(cs),
                    is_spurious_dragon_enabled,
                )
            });

        self.logs.truncate(checkpoint.log_i);
        self.journal.truncate(checkpoint.journal_i);
    }

    /// Performances selfdestruct action.
    /// Transfers balance from address to target. Check if target exist/is_cold
    ///
    /// Note: balance will be lost if address and target are the same BUT when
    /// current spec enables Cancun, this happens only when the account associated to address
    /// is created in the same tx
    ///
    /// references:
    ///  * <https://github.com/ethereum/go-ethereum/blob/141cd425310b503c5678e674a8c3872cf46b7086/core/vm/instructions.go#L832-L833>
    ///  * <https://github.com/ethereum/go-ethereum/blob/141cd425310b503c5678e674a8c3872cf46b7086/core/state/statedb.go#L449>
    ///  * <https://eips.ethereum.org/EIPS/eip-6780>
    #[inline]
    pub fn selfdestruct<DB: Database>(
        &mut self,
        address: Address,
        target: Address,
        db: &mut DB,
    ) -> Result<StateLoad<SelfDestructResult>, DB::Error> {
        let spec_id = self.spec_id;
        let account_load = self.load_account(target, db)?;
        let is_cold = account_load.is_cold;
        let is_empty = account_load.state_clear_aware_is_empty(spec_id);

        if address != target {
            // Both accounts are loaded before this point, `address` as we execute its contract.
            // and `target` at the beginning of the function.
            let acc_balance = self.state.get_mut(&address).unwrap().info.balance;

            let target_account = self.state.get_mut(&target).unwrap();
            Self::touch_account(self.journal.last_mut().unwrap(), &target, target_account);
            target_account.info.balance += acc_balance;
        }

        let acc = self.state.get_mut(&address).unwrap();
        let balance = acc.info.balance;
        let previously_destroyed = acc.is_selfdestructed();
        let is_cancun_enabled = self.spec_id.is_enabled_in(SpecId::CANCUN);

        // EIP-6780 (Cancun hard-fork): selfdestruct only if contract is created in the same tx
        let journal_entry = if acc.is_created() || !is_cancun_enabled {
            acc.mark_selfdestruct();
            acc.info.balance = U256::ZERO;
            Some(JournalEntry::AccountDestroyed {
                address,
                target,
                was_destroyed: previously_destroyed,
                had_balance: balance.into(),
            })
        } else if address != target {
            acc.info.balance = U256::ZERO;
            Some(JournalEntry::BalanceTransfer {
                from: address,
                to: target,
                balance,
            })
        } else {
            // State is not changed:
            // * if we are after Cancun upgrade and
            // * SelfDestruct account that is created in the same transaction and
            // * Specify the target is same as selfdestructed account. The balance stays unchanged.
            None
        };

        if let Some(entry) = journal_entry {
            self.journal.last_mut().unwrap().push(entry);
        };

        Ok(StateLoad {
            data: SelfDestructResult {
                had_value: !balance.is_zero(),
                target_exists: !is_empty,
                previously_destroyed,
            },
            is_cold,
        })
    }

    /// Initial load of account. This load will not be tracked inside journal
    #[inline]
    pub fn initial_account_load<DB: Database>(
        &mut self,
        address: Address,
        storage_keys: impl IntoIterator<Item = Bytes32>,
        db: &mut DB,
    ) -> Result<&mut Account, DB::Error> {
        // load or get account.
        let account = match self.state.entry(address) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(vac) => vac.insert(
                db.basic(address)?
                    .map(|i| i.into())
                    .unwrap_or(Account::new_not_existing()),
            ),
        };
        // preload storages.
        for storage_key in storage_keys.into_iter() {
            if let Entry::Vacant(entry) = account.storage.entry(storage_key.to_u256()) {
                let storage = db.storage(address, storage_key.to_u256())?;
                entry.insert(StorageSlot::new(storage));
            }
        }
        Ok(account)
    }

    /// load account into memory. return if it is cold or warm accessed
    pub fn load_account<DB: Database>(
        &mut self,
        address: Address,
        db: &mut DB,
    ) -> Result<StateLoad<&mut Account>, DB::Error> {
        let load = match self.state.entry(address) {
            Entry::Occupied(entry) => {
                let account = entry.into_mut();
                let is_cold = account.mark_warm();
                StateLoad {
                    data: account,
                    is_cold,
                }
            }
            Entry::Vacant(vac) => {
                let account = if let Some(account) = db.basic(address)? {
                    account.into()
                } else {
                    Account::new_not_existing()
                };

                // precompiles are warm loaded so we need to take that into account
                let is_cold = !self.warm_preloaded_addresses.contains(&address);

                StateLoad {
                    data: vac.insert(account),
                    is_cold,
                }
            }
        };

        // journal loading of cold account.
        if load.is_cold {
            self.journal
                .last_mut()
                .unwrap()
                .push(JournalEntry::AccountWarmed { address });
        }

        Ok(load)
    }

    pub fn load_account_delegated<DB: Database>(
        &mut self,
        address: Address,
        db: &mut DB,
    ) -> Result<StateLoad<AccountLoad>, DB::Error> {
        let spec_id = self.spec_id;
        let is_eip7702_enabled = spec_id.is_enabled_in(SpecId::PRAGUE);
        let account = self.load_account_optional(db, address, is_eip7702_enabled)?;
        let is_empty = account.state_clear_aware_is_empty(spec_id);
        let mut account_load = StateLoad::new(
            AccountLoad {
                is_delegate_account_cold: None,
                is_empty,
            },
            account.is_cold,
        );
        // load delegate code if account is EIP-7702
        if let Some(EVMBytecode::Eip7702(code)) = &account.info.code {
            let address = code.address();
            let delegate_account = self.load_account(address, db)?;
            account_load.data.is_delegate_account_cold = Some(delegate_account.is_cold);
        }
        Ok(account_load)
    }

    /// Loads code.
    #[inline]
    pub fn load_code<DB: Database>(
        &mut self,
        db: &mut DB,
        address: Address,
    ) -> Result<StateLoad<&mut Account>, DB::Error> {
        self.load_account_optional(db, address, true)
    }

    /// Loads code
    #[inline]
    pub fn load_account_optional<DB: Database>(
        &mut self,
        db: &mut DB,
        address: Address,
        load_code: bool,
    ) -> Result<StateLoad<&mut Account>, DB::Error> {
        let load = match self.state.entry(address) {
            Entry::Occupied(entry) => {
                let account = entry.into_mut();
                let is_cold = account.mark_warm();
                StateLoad {
                    data: account,
                    is_cold,
                }
            }
            Entry::Vacant(vac) => {
                let account = if let Some(account) = db.basic(address)? {
                    account.into()
                } else {
                    Account::new_not_existing()
                };

                // Precompiles among some other account are warm loaded so we need to take that into account
                let is_cold = !self.warm_preloaded_addresses.contains(&address);

                StateLoad {
                    data: vac.insert(account),
                    is_cold,
                }
            }
        };
        // journal loading of cold account.
        if load.is_cold {
            self.journal
                .last_mut()
                .unwrap()
                .push(JournalEntry::AccountWarmed { address });
        }
        if load_code {
            let info = &mut load.data.info;
            if info.code.is_none() {
                let code = if info.code_hash == KECCAK_EMPTY {
                    Bytecode::empty()
                } else {
                    db.code_by_hash(info.code_hash)?
                };
                info.code = Some(code);
            }
        }

        Ok(load)
    }

    /// Load storage slot
    ///
    /// # Panics
    ///
    /// Panics if the account is not present in the state.
    pub fn sload<DB: Database>(
        &mut self,
        address: Address,
        key: Bytes32,
        db: &mut DB,
    ) -> Result<StateLoad<Bytes32>, DB::Error> {
        // Check account is warm
        let account = self.state.get_mut(&address).unwrap();
        // Only if account is created in this tx we can assume that storage is empty.
        let is_newly_created = account.is_created();
        let (value, is_cold) = match account.storage.entry(key.to_u256()) {
            Entry::Occupied(occ) => {
                let slot = occ.into_mut();
                let is_cold = slot.mark_warm();
                (slot.present_value, is_cold)
            }
            Entry::Vacant(vac) => {
                // if storage was cleared, we don't need to ping db.
                let value = if is_newly_created {
                    U256::ZERO
                } else {
                    db.storage(address, key.to_u256())?
                };

                vac.insert(StorageSlot::new(value));

                (value, true)
            }
        };

        if is_cold {
            // add it to journal as cold loaded.
            self.journal
                .last_mut()
                .unwrap()
                .push(JournalEntry::StorageWarmed { address, key });
        }

        Ok(StateLoad::new(Bytes32::from(value), is_cold))
    }

    /// Stores storage slot.
    /// And returns (original,present,new) slot value.
    ///
    /// Note:
    ///
    /// account should already be present in our state.
    pub fn sstore<DB: Database>(
        &mut self,
        address: Address,
        key: Bytes32,
        new: Bytes32,
        db: &mut DB,
    ) -> Result<StateLoad<SStoreResult>, DB::Error> {
        // assume that acc exists and load the slot.
        let present = self.sload(address, key, db)?;
        let acc = self.state.get_mut(&address).unwrap();

        // if there is no original value in dirty return present value, that is our original.
        let slot = acc.storage.get_mut(key.as_u256()).unwrap();

        // new value is same as present, we don't need to do anything
        if present.data == new {
            return Ok(StateLoad::new(
                SStoreResult::Slot(SStoreSlot {
                    original_value: Bytes32::from(slot.original_value()),
                    present_value: present.data,
                    new_value: new,
                }),
                present.is_cold,
            ));
        }

        self.journal
            .last_mut()
            .unwrap()
            .push(JournalEntry::StorageChanged {
                address,
                key,
                had_value: present.data,
            });
        // insert value into present state.
        slot.present_value = new.to_u256();
        Ok(StateLoad::new(
            SStoreResult::Slot(SStoreSlot {
                original_value: Bytes32::from(slot.original_value()),
                present_value: present.data,
                new_value: new,
            }),
            present.is_cold,
        ))
    }

    /// Read transient storage tied to the account.
    ///
    /// EIP-1153: Transient storage opcodes
    #[inline]
    pub fn tload(&mut self, address: Address, key: Bytes32) -> Bytes32 {
        self.transient_storage
            .get(&(address, key))
            .copied()
            .unwrap_or_default()
    }

    /// Store transient storage tied to the account.
    ///
    /// If values is different add entry to the journal
    /// so that old state can be reverted if that action is needed.
    ///
    /// EIP-1153: Transient storage opcodes
    #[inline]
    pub fn tstore(&mut self, address: Address, key: Bytes32, new: Bytes32) {
        let had_value = if new.as_u256().is_zero() {
            // if new values is zero, remove entry from transient storage.
            // if previous values was some insert it inside journal.
            // If it is none nothing should be inserted.
            self.transient_storage.remove(&(address, key))
        } else {
            // insert values
            let previous_value = self
                .transient_storage
                .insert((address, key), new)
                .unwrap_or_default();

            // check if previous value is same
            if previous_value != new {
                // if it is different, insert previous values inside journal.
                Some(previous_value)
            } else {
                None
            }
        };

        if let Some(had_value) = had_value {
            // insert in journal only if value was changed.
            self.journal
                .last_mut()
                .unwrap()
                .push(JournalEntry::TransientStorageChange {
                    address,
                    key,
                    had_value,
                });
        }
    }

    /// push log into subroutine
    #[inline]
    pub fn log(&mut self, log: Log) {
        self.logs.push(log);
    }
}

/// Journal entries that are used to track changes to the state and are used to revert it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JournalEntry {
    /// Used to mark account that is warm inside VM in regards to EIP-2929 AccessList.
    /// Action: We will add Account to state.
    /// Revert: we will remove account from state.
    AccountWarmed { address: Address },
    /// Mark account to be destroyed and journal balance to be reverted
    /// Action: Mark account and transfer the balance
    /// Revert: Unmark the account and transfer balance back
    AccountDestroyed {
        address: Address,
        target: Address,
        was_destroyed: bool, // if account had already been destroyed before this journal entry
        had_balance: Bytes32,
    },
    /// Loading account does not mean that account will need to be added to MerkleTree (touched).
    /// Only when account is called (to execute contract or transfer balance) only then account is made touched.
    /// Action: Mark account touched
    /// Revert: Unmark account touched
    AccountTouched { address: Address },
    /// Transfer balance between two accounts
    /// Action: Transfer balance
    /// Revert: Transfer balance back
    BalanceTransfer {
        from: Address,
        to: Address,
        balance: U256,
    },
    /// Increment nonce
    /// Action: Increment nonce by one
    /// Revert: Decrement nonce by one
    NonceChange {
        address: Address, //geth has nonce value,
    },
    /// Create account:
    /// Actions: Mark account as created
    /// Revert: Unmart account as created and reset nonce to zero.
    AccountCreated { address: Address },
    /// Entry used to track storage changes
    /// Action: Storage change
    /// Revert: Revert to previous value
    StorageChanged {
        address: Address,
        key: Bytes32,
        had_value: Bytes32,
    },
    /// Entry used to track storage warming introduced by EIP-2929.
    /// Action: Storage warmed
    /// Revert: Revert to cold state
    StorageWarmed { address: Address, key: Bytes32 },
    /// It is used to track an EIP-1153 transient storage change.
    /// Action: Transient storage changed.
    /// Revert: Revert to previous value.
    TransientStorageChange {
        address: Address,
        key: Bytes32,
        had_value: Bytes32,
    },
    /// Code changed
    /// Action: Account code changed
    /// Revert: Revert to previous bytecode.
    CodeChange { address: Address },
}

/// SubRoutine checkpoint that will help us to go back from this
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct JournalCheckpoint {
    log_i: usize,
    journal_i: usize,
}
