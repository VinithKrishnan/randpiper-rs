use serde::{Deserialize, Serialize};

use super::super::View;
use super::block::*;
use crate::{protocol::*, WireReady};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum VoteType {
    /// NoProgress Blame is sent for not proposing in time
    /// It contains
    /// 1) the leader
    /// 2) the view
    NoProgressBlame(Replica, View),
    /// Equivocation Blame is sent when two equivocating proposals are heard
    /// It contains
    /// 1) The leader who equivocated
    /// 2) The two equivocating blocks
    EquivcationBlame(Replica, Block, Block),
    /// A vote is sent when acknowledging a block
    /// It contains
    /// 1) The hash of the block it is voting
    Vote(Vec<u8>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Vote {
    pub msg: VoteType,
    pub origin: Replica,
    pub auth: Vec<u8>,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Certificate {
    pub votes: Vec<Vote>,
}

impl Certificate {
    pub fn empty_cert() -> Self {
        Certificate { votes: Vec::new() }
    }
}

impl std::default::Default for Certificate {
    fn default() -> Self {
        Certificate::empty_cert()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Transaction {
    pub data: Vec<u8>,
    pub request: Vec<u8>,
}

impl Transaction {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let c: Transaction = flexbuffers::from_slice(&bytes).expect("failed to decode the block");
        return c;
    }
}

impl WireReady for Transaction {}
