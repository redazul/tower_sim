use crate::node::THRESHOLD;
use crate::tower::{Slot, Tower, Vote};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

pub const NUM_NODES: usize = 997;
pub const SUBCOMMITTEE_EPOCH: usize = 64;
pub const SUBCOMMITTEE_SIZE: usize = 200;
pub type ID = usize;

pub struct Subcommittee {
    //the current primary and secondary
    pub primary: HashSet<ID>,
    pub secondary: HashSet<ID>,
    // number of times supermajority roots have increased
    // this squashes ranges of increases into 1
    pub num_super_roots: usize,
    pub parent_num_super_roots: usize,
    pub super_root: Slot,
    pub parent_super_root: Slot,
}
pub struct Bank {
    pub nodes: Vec<Tower>,
    pub slot: Slot,
    pub parent: Slot,
    pub frozen: bool,
    pub children: Vec<Slot>,
    pub subcom: Subcommittee,
}

pub struct Block {
    pub slot: Slot,
    pub parent: Slot,
    pub votes: Vec<(ID, Vec<Vote>)>,
}

pub struct Banks {
    pub fork_map: HashMap<Slot, Bank>,
    pub fork_weights: HashMap<Slot, usize>,
    pub lowest_root: Vote,
}

impl Default for Banks {
    fn default() -> Self {
        let bank_zero = Bank::zero();
        let mut fork_map = HashMap::new();
        fork_map.insert(0, bank_zero);
        Self {
            fork_map,
            fork_weights: HashMap::new(),
            lowest_root: Vote::zero(),
        }
    }
}

pub enum Phase {
    SecondaryRotationB,
    PrimaryA2B,
    SecondaryRotationA,
    PrimaryB2A,
}

impl Default for Subcommittee {
    fn default() -> Self {
        let primary = Self::calc_subcommittee(0);
        let secondary = primary.clone();
        Self {
            parent_super_root: 0,
            super_root: 0,
            num_super_roots: 0,
            parent_num_super_roots: 0,
            primary,
            secondary,
        }
    }
}

impl Subcommittee {
    pub fn child(self: &Self) -> Self {
        Self {
            parent_super_root: self.super_root,
            super_root: self.super_root,
            num_super_roots: self.num_super_roots,
            //the new subcomittee epoch is activated
            //on the child bank after the parent is frozen
            parent_num_super_roots: self.num_super_roots,
            primary: self.primary.clone(),
            secondary: self.secondary.clone(),
        }
    }
    pub fn init_child(&mut self, parent: &Self) {
        if self.subcommittee_epoch() != parent.subcommittee_epoch() {
            let epoch = self.subcommittee_epoch();
            match self.subcommittee_phase() {
                Phase::SecondaryRotationB => self.secondary = Self::calc_subcommittee(epoch),
                Phase::PrimaryA2B => std::mem::swap(&mut self.primary, &mut self.secondary),
                Phase::SecondaryRotationA => self.secondary = Self::calc_subcommittee(epoch),
                Phase::PrimaryB2A => std::mem::swap(&mut self.primary, &mut self.secondary),
            }
        }
    }

    pub fn freeze(&mut self, super_root: Slot) {
        self.super_root = super_root;
        if self.super_root != self.parent_super_root {
            self.num_super_roots = self.num_super_roots + 1;
        }
    }

    fn hash(val: u64) -> u64 {
        let mut h = DefaultHasher::new();
        val.hash(&mut h);
        h.finish()
    }

    fn calc_subcommittee(epoch: usize) -> HashSet<ID> {
        let mut set = HashSet::new();
        let mut seed = Self::hash(epoch as u64);
        for _ in 0..SUBCOMMITTEE_SIZE {
            set.insert(seed as usize % SUBCOMMITTEE_SIZE);
            seed = Self::hash(seed);
        }
        set
    }
    fn subcommittee_epoch(&self) -> usize {
        self.parent_num_super_roots / SUBCOMMITTEE_EPOCH
    }

    fn subcommittee_phase(&self) -> Phase {
        match self.subcommittee_epoch() % 4 {
            0 => Phase::SecondaryRotationB,
            1 => Phase::PrimaryA2B,
            2 => Phase::SecondaryRotationA,
            3 => Phase::PrimaryB2A,
            _ => panic!("invalid"),
        }
    }
}

impl Banks {
    pub fn apply(&mut self, block: &Block) {
        assert!(self.fork_map.get(&block.slot).is_none());
        let parent = self.fork_map.get_mut(&block.parent).unwrap();
        let mut bank = parent.child(block.slot);
        let mut fork: HashSet<_> = self.compute_fork(block.parent).into_iter().collect();
        fork.insert(bank.slot);
        bank.apply(block, &fork);
        let lowest_root = bank.lowest_root();
        assert!(self.fork_map.get(&bank.slot).is_none());
        let mut max_root = 0;
        for n in bank.nodes.iter() {
            if n.root.slot > max_root {
                max_root = n.root.slot;
            }
        }
        self.fork_map.insert(bank.slot, bank);
        if lowest_root.slot > self.lowest_root.slot {
            println!("ROOT DISTANCE {}", max_root - lowest_root.slot);
            println!(
                "LOWEST ROOT UPDATE {:?} {:?} MAX: {}",
                self.lowest_root, lowest_root, max_root
            );
            self.lowest_root = lowest_root;
            self.gc();
        }
        self.build_fork_weights();
    }

    pub fn compute_fork(&self, slot: Slot) -> Vec<Slot> {
        let mut fork = vec![slot];
        loop {
            let last = fork.last().unwrap();
            if let Some(b) = self.fork_map.get(last) {
                if *last == b.parent {
                    break;
                }
                fork.push(b.parent)
            } else {
                break;
            }
        }
        fork
    }

    //only keep forks that are connected to root
    fn gc(&mut self) {
        let mut valid = vec![];

        println!("START GC {:?}", self.lowest_root);
        let mut children = vec![self.lowest_root.slot];
        while !children.is_empty() {
            let slot = children.pop().unwrap();
            valid.push(slot);
            let bank = self.fork_map.get(&slot).unwrap();
            children.extend_from_slice(&bank.children);
        }
        let mut new_banks = HashMap::new();
        for v in valid {
            new_banks.insert(v, self.fork_map.remove(&v).unwrap());
        }
        self.fork_map = new_banks;
    }
    /// A validator V's vote on an ancestor X counts towards a descendant
    /// Y even if the validator is not locked out on X at Y anymore,
    /// as long as X is the latest vote observed from this validator V
    pub fn build_fork_weights(&mut self) {
        //each validators latest votes
        let mut latest_votes: HashMap<ID, Slot> = HashMap::new();
        for v in self.fork_map.values() {
            v.latest_votes(&mut latest_votes);
        }
        //total stake voting per slot
        let mut slot_votes: HashMap<Slot, usize> = HashMap::new();
        for (_, v) in &latest_votes {
            let e = slot_votes.entry(*v).or_insert(0);
            *e = *e + 1;
        }
        //stake weight is inherited from the parent
        let mut weights: HashMap<Slot, usize> = HashMap::new();
        let mut children = vec![self.lowest_root.slot];
        while !children.is_empty() {
            let child = children.pop().unwrap();
            let bank = self.fork_map.get(&child).unwrap();
            children.extend_from_slice(&bank.children);
            let parent_weight = *weights.get(&bank.parent).unwrap_or(&0);
            let e = weights.entry(child).or_insert(parent_weight);
            *e = *e + *slot_votes.get(&child).unwrap_or(&0);
        }
        self.fork_weights = weights;
    }
}

impl Bank {
    pub fn zero() -> Self {
        let mut nodes = vec![];
        for _ in 0..NUM_NODES {
            nodes.push(Tower::default());
        }
        Bank {
            frozen: true,
            nodes,
            slot: 0,
            parent: 0,
            subcom: Subcommittee::default(),
            children: vec![],
        }
    }
    pub fn child(&mut self, slot: Slot) -> Self {
        assert!(self.frozen);
        let mut b = Bank {
            nodes: self.nodes.clone(),
            slot,
            parent: self.slot,
            children: vec![],
            subcom: self.subcom.child(),
            frozen: false,
        };
        b.subcom.init_child(&self.subcom);
        self.children.push(slot);
        b
    }
    pub fn apply(&mut self, block: &Block, fork: &HashSet<Slot>) {
        assert!(!self.frozen);
        assert_eq!(self.slot, block.slot);
        assert_eq!(self.parent, block.parent);
        for (id, votes) in &block.votes {
            for v in votes {
                assert!(
                    fork.contains(&v.slot),
                    "proposed vote is not in the bank's fork {:?} {}",
                    fork,
                    v.slot
                );
                let _e = self.nodes[*id].apply(v);
            }
        }
        let super_root = self.calc_super_root().slot;
        self.subcom.freeze(super_root);
        self.frozen = true;
    }
    pub fn calc_threshold_slot(&self, mult: u64, vote: &Vote) -> usize {
        let count: usize = self
            .nodes
            .iter()
            .map(|n| {
                //alredy rooted
                if n.root.slot >= vote.slot {
                    return 1;
                }
                for v in &n.votes {
                    if vote.lockout == 1 << THRESHOLD && v.slot >= vote.slot {
                        return 1;
                    }
                    //check if the node has a higher vote with at least 1/2 the lockout
                    if v.slot >= vote.slot
                        && (v.slot + (mult * v.lockout)) >= (vote.slot + vote.lockout)
                    {
                        return 1;
                    }
                }
                0
            })
            .sum();
        count
    }
    pub fn threshold_slot(&self, vote: &Vote) -> bool {
        self.calc_threshold_slot(1 << THRESHOLD, vote) > (2 * NUM_NODES) / 3
    }
    pub fn calc_super_root(&self) -> Vote {
        let mut roots: Vec<_> = self.nodes.iter().map(|n| n.root).collect();
        roots.sort_by_key(|x| x.slot);
        //2/3 of the nodes are at least at this root
        roots[NUM_NODES / 3]
    }

    fn lowest_root(&self) -> Vote {
        let mut roots: Vec<_> = self.nodes.iter().map(|n| n.root).collect();
        roots.sort_by_key(|x| x.slot);
        roots[0]
    }

    //get the latest votes from each node
    pub fn latest_votes(&self, latest_votes: &mut HashMap<ID, Slot>) {
        for (i, n) in self.nodes.iter().enumerate() {
            let latest = n.latest_vote().unwrap_or(&n.root);
            let e = latest_votes.entry(i).or_insert(latest.slot);
            if *e < latest.slot {
                *e = latest.slot;
            }
        }
    }
}
