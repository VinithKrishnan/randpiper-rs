use serde::{Deserialize, Serialize};

use super::Certificate;
use crate::{Propose, Replica, Vote, WireReady};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum ProtocolMsg {
    Certificate(Certificate),
    Identify(Replica),
    Proposal(Propose),
    Blame(Vote),
}

impl ProtocolMsg {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let c: ProtocolMsg =
            flexbuffers::from_slice(&bytes).expect("failed to decode the protocol message");
        return c;
    }
}

impl WireReady for ProtocolMsg {}
