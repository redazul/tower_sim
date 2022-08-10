use crate::bank::{Bank, Block, ID, NUM_NODES};
use crate::tower::{Slot, Tower, Vote};
use std::collections::HashMap;

const THRESHOLD: usize = 8;

pub struct Node {
    id: ID,
    pub supermajority_root: Vote,
    banks: HashMap<Slot, Bank>,
    tower: Tower,
}

impl Node {
    pub fn apply(&mut self, block: &Block) {
        assert!(self.banks.get(&block.slot).is_none());
        let parent = self.banks.get_mut(&block.parent).unwrap();
        let mut bank = parent.child(self.id, block.slot);
        bank.apply(block);
        let root = bank.supermajority_root();
        assert!(root.slot >= self.supermajority_root.slot);
        if root.slot != self.supermajority_root.slot {
            self.gc();
        }
        self.supermajority_root = root;
        self.banks.insert(bank.slot, bank);
    }
    //only keep forks that are connected to root
    pub fn gc(&mut self) {
        let mut valid = vec![];
        let mut children = vec![self.supermajority_root.slot];
        while !children.is_empty() {
            let slot = children.pop().unwrap();
            valid.push(slot);
            let bank = self.banks.get(&slot).unwrap();
            children.extend_from_slice(&bank.children);
        }
        let mut new_banks = HashMap::new();
        for v in valid {
            new_banks.insert(v, self.banks.remove(&v).unwrap());
        }
        self.banks = new_banks;
    }

    /// A validator V's vote on an ancestor X counts towards a descendant
    /// Y even if the validator is not locked out on X at Y anymore,
    /// as long as X is the latest vote observed from this validator V
    pub fn fork_weights(&self) -> HashMap<Slot, usize> {
        //each validators latest votes
        let mut latest_votes: HashMap<ID, Slot> = HashMap::new();
        for v in self.banks.values() {
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
        let mut children = vec![self.supermajority_root.slot];
        while !children.is_empty() {
            let b = children.pop().unwrap();
            let bank = self.banks.get(&b).unwrap();
            children.extend_from_slice(&bank.children);
            let parent_weight = *self
                .banks
                .get(&bank.parent)
                .map(|parent| weights.get(&parent.parent))
                .flatten()
                .unwrap_or(&0);
            let e = weights.entry(bank.parent).or_insert(parent_weight);
            *e = *e + *slot_votes.get(&bank.parent).unwrap_or(&0);
        }
        weights
    }
    fn threshold_check(&self, tower: &Tower) -> bool {
        let vote = tower.votes.front().unwrap();
        let bank = self.banks.get(&vote.slot).unwrap();
        for v in &tower.votes {
            if v.lockout > 1<<THRESHOLD {
                if !bank.supermajority_slot(&vote) {
                    return false;
                }
            }
        }
        true
    }

    fn compute_fork(&self, slot: Slot) -> Vec<Slot> {
        let mut fork = vec![slot];
        loop {
            if let Some(b) = self.banks.get(fork.last().unwrap()) {
                fork.push(b.parent)
            } else {
                break;
            }
        }
        fork
    }

    fn optimistic_conf_check(&self, new_fork: &[Slot], fork_weights: &HashMap<Slot, usize>) -> bool {
        // no votes left in tower
        if self.tower.votes.front().is_none() {
            return true;
        }
        let last_vote = self.tower.votes.front().unwrap();
        // if the last vote is a decendant of the new fork
        // no switching proof is necessary
        if new_fork.iter().find(|x| **x == last_vote.slot).is_some() {
            return true;
        } 
        //all the recent forks but those decending from the last vote must have > 1/3 votes
        let mut total = 0;
        for (slot, stake) in fork_weights {
            if *slot <= last_vote.slot {
                //slot is older than last vote
                continue;
            }
            let fork = self.compute_fork(*slot);
            if fork.iter().find(|x| **x == last_vote.slot).is_none() {
                //slot is not a child of the last vote
                total += stake;
            }
        }
        total > NUM_NODES / 3
    }
    pub fn vote(&mut self) -> Option<Vote> {
        let weights = self.fork_weights();
        let heaviest_slot = weights
            .iter()
            .map(|(x, y)| (y, x))
            .max()
            .map(|(_, y)| *y)
            .unwrap_or(0);
        //recursively find the fork for the heaviest slot
        let fork = self.compute_fork(heaviest_slot);
        let mut tower = self.tower.clone();
        let vote = Vote {
            slot: heaviest_slot,
            lockout: 2,
        };
        //apply this vote and expire all the old votes
        tower.apply(&vote);
        //the most recent unexpired vote must be in the heaviest fork
        //of this is the first vote in tower
        if tower.votes.len() > 1 && fork.iter().find(|x| **x == tower.votes[1].slot).is_none() {
            return None;
        }
        if !self.threshold_check(&tower) {
            return None;
        }
        if !self.optimistic_conf_check(&fork, &weights) {
            return None;
        }
        self.tower = tower;
        return Some(vote);
    }
}
