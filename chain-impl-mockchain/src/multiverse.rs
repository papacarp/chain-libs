//! Multiverse
//!
//! This is a multi temporal store, where the timeline is accessible by HeaderId
//! and multiple timelines are possible.
//!
//! For now this only track block at the headerhash level, and doesn't order them
//! temporaly, leaving no way to do garbage collection

use crate::block::ChainLength;
use crate::header::HeaderId;
use crate::ledger::Ledger;
use chain_storage::store::BlockStore;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hint::unreachable_unchecked;
use std::sync::{Arc, Weak};

//
// The multiverse is characterized by a single origin and multiple state of a given time
//
//          [root A]
//        ,o            ,-o-o--o [root B]
//       /             /
// o----o----o--o--o--o-o-o-o-oooo [root E]
//                  \
//                   `-o--o [root C]
//                      \
//                      `----o-o-oo [root F]
//
// +------------------------------+-----> time
// t=0                            t=latest known
//
pub struct Multiverse<State> {
    states_by_hash: HashMap<HeaderId, GcEntry<State>>,
    states_by_chain_length: BTreeMap<ChainLength, HashSet<HeaderId>>, // FIXME: use multimap?
}

/// Keep all states that are this close to the longest chain.
const SUFFIX_TO_KEEP: u32 = 50;

/// A RAII wrapper around a block identifier and the state pointer
/// that keeps the state corresponding to the block pinned in memory.
#[derive(Clone)]
pub struct Ref<State> {
    hash: HeaderId,
    state: Arc<State>,
}

impl<State> Ref<State> {
    fn new(hash: HeaderId, state: Arc<State>) -> Self {
        Ref { hash, state }
    }

    pub fn id(&self) -> &HeaderId {
        &self.hash
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn state_arc(&self) -> Arc<State> {
        self.state.clone()
    }
}

enum GcEntry<State> {
    Retained(Arc<State>),
    Collectable(Weak<State>),
}

impl<State> GcEntry<State> {
    fn get(&self) -> Option<Arc<State>> {
        match self {
            GcEntry::Retained(arc) => Some(Arc::clone(arc)),
            GcEntry::Collectable(weak) => weak.upgrade(),
        }
    }

    fn collect(&mut self) -> bool {
        if let GcEntry::Retained(arc) = self {
            let weak = Arc::downgrade(arc);
            *self = GcEntry::Collectable(weak);
        }
        match self {
            GcEntry::Collectable(weak) => weak.strong_count() == 0,
            GcEntry::Retained(_) => unsafe { unreachable_unchecked() },
        }
    }
}

impl<State> Multiverse<State> {
    pub fn new() -> Self {
        Multiverse {
            states_by_hash: HashMap::new(),
            states_by_chain_length: BTreeMap::new(),
        }
    }

    pub fn get(&self, k: &HeaderId) -> Option<Arc<State>> {
        let entry = self.states_by_hash.get(k)?;
        entry.get()
    }

    pub fn get_ref(&self, k: &HeaderId) -> Option<Ref<State>> {
        self.get(k).map(|state| Ref::new(k.clone(), state))
    }

    /// Return the number of states stored in memory.
    pub fn nr_states(&self) -> usize {
        self.states_by_hash.len()
    }

    /// Add a state to the multiverse. Return a Ref object that
    /// pins the state in memory.
    pub fn insert(&mut self, chain_length: ChainLength, k: HeaderId, st: State) -> Ref<State> {
        self.states_by_chain_length
            .entry(chain_length)
            .or_insert_with(|| HashSet::new())
            .insert(k.clone());
        let state = Arc::new(st);
        self.states_by_hash
            .insert(k.clone(), GcEntry::Retained(state.clone()));
        Ref::new(k, state)
    }
}

impl Multiverse<Ledger> {
    /// Add a state to the multiverse. Return a `Ref` object that
    /// pins the state into memory.
    pub fn add(&mut self, k: HeaderId, st: Ledger) -> Ref<Ledger> {
        self.insert(st.chain_length(), k, st)
    }

    /// Once the state are old in the timeline, they are less
    /// and less likely to be used anymore, so we leave
    /// a gap between different version that gets bigger and bigger
    pub fn gc(&mut self) {
        let longest_chain = match self.states_by_chain_length.keys().next_back() {
            Some(len) => *len,
            None => return,
        };

        if let Some(gc_threshold_length) = longest_chain.nth_ancestor(SUFFIX_TO_KEEP) {
            let mut scan_length = ChainLength(0);
            let mut to_keep = ChainLength(0);

            // Keep states close to the current longest
            // chain. FIXME: we should keep only the state that is
            // an ancestor of the current longest chain. However,
            // checking ancestry requires access to BlockStore.
            let states_by_hash = &mut self.states_by_hash;
            while let Some((&chain_length, hashes)) = self
                .states_by_chain_length
                .range_mut(scan_length..gc_threshold_length)
                .next()
            {
                // Keep states in gaps that get exponentially smaller
                // as they get closer to the longest chain.
                let keep = if chain_length >= to_keep {
                    to_keep = ChainLength(chain_length.0 + (longest_chain.0 - chain_length.0) / 2);
                    true
                } else {
                    // Keep states that are kept alive by Ref values.
                    hashes.retain(|k| {
                        use std::collections::hash_map::Entry::*;

                        match states_by_hash.entry(k.clone()) {
                            Occupied(mut entry) => {
                                if entry.get_mut().collect() {
                                    entry.remove();
                                    false
                                } else {
                                    true
                                }
                            }
                            Vacant(_) => panic!("dangling state index entry"),
                        }
                    });
                    !hashes.is_empty()
                };
                if !keep {
                    self.states_by_chain_length.remove(&chain_length);
                }
                scan_length = chain_length.increase();
            }
        }
    }

    /// Get the chain state at block 'k' from memory if present;
    /// otherwise reconstruct it by reading blocks from storage and
    /// applying them to the nearest ancestor state that we do have.
    pub fn get_from_storage<S: BlockStore<Block = crate::block::Block>>(
        &mut self,
        k: HeaderId,
        store: &S,
    ) -> Result<Ref<Ledger>, chain_storage::error::Error> {
        if let Some(r) = self.get_ref(&k) {
            return Ok(r);
        }

        // Find the most recent ancestor that we have in
        // memory. FIXME: could do a binary search here on the chain
        // length interval between 0 and k.chain_length(), though it
        // doesn't matter much for complexity since we need to apply
        // O(n) blocks anyway.

        let mut blocks_to_apply = vec![];
        let mut cur_hash = k.clone();

        let mut state_ref = loop {
            if cur_hash == HeaderId::zero_hash() {
                panic!("don't know how to reconstruct initial chain state");
            }

            if let Some(state_ref) = self.get_ref(&cur_hash) {
                break state_ref;
            }

            let cur_block_info = store.get_block_info(&cur_hash).unwrap();
            blocks_to_apply.push(cur_hash.clone());
            cur_hash = cur_block_info.parent_id();
        };

        /*
        println!(
            "applying {} blocks to reconstruct state at {}",
            blocks_to_apply.len(),
            k
        );
        */

        for hash in blocks_to_apply.iter().rev() {
            let block = store.get_block(&hash).unwrap().0;
            let header_meta = block.header.to_content_eval_context();
            let state = state_ref.state();
            let state = state
                .apply_block(
                    &state.get_ledger_parameters(),
                    &block.contents,
                    &header_meta,
                )
                .unwrap();
            state_ref = self.add(hash.clone(), state);
        }

        Ok(state_ref)
    }
}

#[cfg(test)]
mod test {
    use super::Multiverse;
    use crate::block::{Block, ConsensusVersion, Contents, ContentsBuilder};
    use crate::config::{Block0Date, ConfigParam};
    use crate::date::BlockDate;
    use crate::fragment::{ConfigParams, Fragment};
    use crate::header::{BlockVersion, HeaderBuilderNew};
    use crate::leadership::bft::LeaderId;
    use crate::ledger::Ledger;
    use crate::milli::Milli;
    use chain_addr::Discrimination;
    use chain_core::property::{Block as _, ChainLength as _};
    use chain_crypto::{Ed25519, SecretKey};
    use chain_storage::store::BlockStore;
    use chain_time::{Epoch, SlotDuration, TimeEra, TimeFrame, Timeline};
    use std::mem;
    use std::time::SystemTime;

    fn apply_block(state: &Ledger, block: &Block) -> Ledger {
        if state.chain_length().0 != 0 {
            assert_eq!(state.chain_length().0 + 1, block.chain_length().0);
        }
        state
            .apply_block(
                &state.get_ledger_parameters(),
                &block.contents,
                &block.header.to_content_eval_context(),
            )
            .unwrap()
    }

    #[test]
    pub fn multiverse() {
        const NUM_BLOCK_PER_EPOCH: u32 = 1000;
        let mut multiverse = Multiverse::new();

        let system_time = SystemTime::UNIX_EPOCH;
        let timeline = Timeline::new(system_time);
        let tf = TimeFrame::new(timeline, SlotDuration::from_secs(10));

        let slot0 = tf.slot0();
        let era = TimeEra::new(slot0, Epoch(0), NUM_BLOCK_PER_EPOCH);

        let leader_key: SecretKey<Ed25519> = SecretKey::generate(rand_core::OsRng);
        let leader_pub_key = leader_key.to_public();

        let mut store = chain_storage::memory::MemoryBlockStore::new();

        let block_ver = BlockVersion::Ed25519Signed;

        let mut ents = ConfigParams::new();
        ents.push(ConfigParam::Discrimination(Discrimination::Test));
        ents.push(ConfigParam::ConsensusVersion(ConsensusVersion::Bft));
        ents.push(ConfigParam::AddBftLeader(LeaderId::from(leader_pub_key)));
        ents.push(ConfigParam::Block0Date(Block0Date(0)));
        ents.push(ConfigParam::SlotDuration(10));
        ents.push(ConfigParam::KESUpdateSpeed(12 * 3600));
        ents.push(ConfigParam::ConsensusGenesisPraosActiveSlotsCoeff(
            Milli::HALF,
        ));
        ents.push(ConfigParam::SlotsPerEpoch(NUM_BLOCK_PER_EPOCH));

        let mut genesis_content = ContentsBuilder::new();
        genesis_content.push(Fragment::Initial(ents));
        let genesis_content = genesis_content.into();

        let mut date = BlockDate::first();
        let genesis_header = HeaderBuilderNew::new(BlockVersion::Genesis, &genesis_content)
            .set_genesis()
            .set_date(date)
            .to_unsigned_header()
            .unwrap()
            .generalize();
        let genesis_block = Block {
            header: genesis_header,
            contents: genesis_content,
        };
        let genesis_state = Ledger::new(genesis_block.id(), genesis_block.contents.iter()).unwrap();
        assert_eq!(genesis_state.chain_length().0, 0);
        store.put_block(&genesis_block).unwrap();
        let _root = multiverse.add(genesis_block.header.id(), genesis_state.clone());

        let mut state = genesis_state;
        let mut _ref = None;
        let mut parent = genesis_block.id();
        let mut ids = vec![];
        for i in 1..10001 {
            date = date.next(&era);
            let contents = Contents::empty();
            let header = HeaderBuilderNew::new(block_ver, &contents)
                .set_parent(&parent, state.chain_length.next())
                .set_date(date)
                .to_bft_builder()
                .unwrap()
                .sign_using(&leader_key)
                .generalize();
            let block = Block { header, contents };
            state = apply_block(&state, &block);
            assert_eq!(state.chain_length().0, i);
            assert_eq!(state.date, block.date());
            store.put_block(&block).unwrap();
            _ref = Some(multiverse.add(block.id(), state.clone()));
            multiverse.gc();
            ids.push(block.header.id());
            parent = block.header.id();
            assert!(
                multiverse.nr_states()
                    <= super::SUFFIX_TO_KEEP as usize + ((i as f32).log2()) as usize
            );
        }

        let ref1 = multiverse
            .get_from_storage(ids[1234].clone(), &store)
            .unwrap();
        let state = ref1.state();
        assert_eq!(state.chain_length().0, 1235);

        let ref2 = multiverse
            .get_from_storage(ids[9999].clone(), &store)
            .unwrap();
        let state = ref2.state();
        assert_eq!(state.chain_length().0, 10000);

        let ref3 = multiverse
            .get_from_storage(ids[9500].clone(), &store)
            .unwrap();
        let state = ref3.state();
        assert_eq!(state.chain_length().0, 9501);

        multiverse.gc();

        {
            let before = multiverse.nr_states();
            mem::drop(ref1);
            mem::drop(ref3);
            multiverse.gc();
            let after = multiverse.nr_states();
            assert_eq!(before, after + 2);
        }
    }
}
