use crate::Height;
use super::Block;
use crypto::EVSSPublicParams381;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Proof {
    pub block_hash: Vec<u8>,
    pub certificate_hash: Vec<u8>,
    pub epoch: Height,
    pub accumulator: EVSSPublicParams381,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Propose {
    pub new_block: Block,
    pub certificate: Certificate,
    pub proof: Proof,
    pub sign: Vec<u8>,
}
