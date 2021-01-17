use super::accumulator::{get_sign, to_shards};
use super::context::Context;
use config::Node;
use crypto::hash::EMPTY_HASH;
use crypto::{CanonicalSerialize, UniformRand};
use crypto::rand::{rngs::StdRng, SeedableRng};
use std::time::Duration;
use std::{convert::TryInto, sync::Arc};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::time;
use types::{Block, Certificate, Content, Height, Propose, ProtocolMsg, Replica, Transaction, Vote, commit_from_bytes};
use util::io::to_bytes;

#[derive(PartialEq, Debug)]
enum Phase {
    Propose,
    DeliverPropose,
    DeliverCommit,
    Vote,
    Commit,
    End,
}

impl Phase {
    pub fn to_string(&self) -> &'static str {
        match self {
            Phase::Propose => "Propose",
            Phase::DeliverPropose => "DeliverPropose",
            Phase::DeliverCommit => "DeliverCommit",
            Phase::Vote => "Vote",
            Phase::Commit => "Commit",
            Phase::End => "End",
        }
    }
}

fn deliver_propose(cx: &mut Context, myid: Replica) {
    let shards = to_shards(
        &to_bytes(&cx.received_propose.as_ref().unwrap())[..],
        cx.num_nodes as usize,
        cx.num_faults as usize,
    );
    cx.propose_gatherer.add_share(
        shards[myid as usize].clone(),
        myid,
        cx.accumulator_pub_params_map.get(&cx.last_leader).unwrap(),
        cx.pub_key_map.get(&cx.last_leader).unwrap(),
        cx.received_propose_sign.clone().unwrap(),
    );
    for i in 0..cx.num_nodes {
        if i != myid {
            cx.net_send
                .send((
                    cx.num_nodes,
                    Arc::new(ProtocolMsg::DeliverPropose(
                        shards[i as usize].clone(),
                        i,
                        cx.received_propose_sign.clone().unwrap(),
                    )),
                ))
                .unwrap();
        }
    }
    cx.net_send
        .send((
            cx.num_nodes,
            Arc::new(ProtocolMsg::DeliverPropose(
                shards[myid as usize].clone(),
                myid,
                cx.received_propose_sign.clone().unwrap(),
            )),
        ))
        .unwrap();
    cx.propose_share_sent = true;
}

fn deliver_vote_cert(cx: &mut Context, myid: Replica) {
    let shards = to_shards(
        &to_bytes(&cx.received_certificate.as_ref().unwrap())[..],
        cx.num_nodes as usize,
        cx.num_faults as usize,
    );
    cx.vote_cert_gatherer.add_share(
        shards[myid as usize].clone(),
        myid,
        cx.accumulator_pub_params_map.get(&cx.last_leader).unwrap(),
        cx.pub_key_map.get(&cx.last_leader).unwrap(),
        cx.received_certificate_sign.clone().unwrap(),
    );
    for i in 0..cx.num_nodes {
        if i != myid {
            cx.net_send
                .send((
                    cx.num_nodes,
                    Arc::new(ProtocolMsg::DeliverVoteCert(
                        shards[i as usize].clone(),
                        i,
                        cx.received_certificate_sign.clone().unwrap(),
                    )),
                ))
                .unwrap();
        }
    }
    cx.net_send
        .send((
            cx.num_nodes,
            Arc::new(ProtocolMsg::DeliverVoteCert(
                shards[myid as usize].clone(),
                myid,
                cx.received_certificate_sign.clone().unwrap(),
            )),
        ))
        .unwrap();
    cx.vote_cert_share_sent = true;
}

fn deliver_commit(cx: &mut Context, myid: Replica) {
    let shards = to_shards(
        &to_bytes(&cx.received_commit.as_ref().unwrap())[..],
        cx.num_nodes as usize,
        cx.num_faults as usize,
    );
    cx.propose_gatherer.add_share(
        shards[myid as usize].clone(),
        myid,
        cx.accumulator_pub_params_map.get(&cx.next_leader()).unwrap(),
        cx.pub_key_map.get(&cx.next_leader()).unwrap(),
        cx.received_commit_sign.clone().unwrap(),
    );
    for i in 0..cx.num_nodes {
        if i != myid {
            cx.net_send
                .send((
                    cx.num_nodes,
                    Arc::new(ProtocolMsg::DeliverCommit(
                        shards[i as usize].clone(),
                        i,
                        cx.received_commit_sign.clone().unwrap(),
                    )),
                ))
                .unwrap();
        }
    }
    cx.net_send
        .send((
            cx.num_nodes,
            Arc::new(ProtocolMsg::DeliverCommit(
                shards[myid as usize].clone(),
                myid,
                cx.received_commit_sign.clone().unwrap(),
            )),
        ))
        .unwrap();
    cx.commit_share_sent = true;
}

pub async fn reactor(
    config: &Node,
    is_client_apollo_enabled: bool,
    net_send: UnboundedSender<(Replica, Arc<ProtocolMsg>)>,
    mut net_recv: UnboundedReceiver<(Replica, ProtocolMsg)>,
    _cli_send: UnboundedSender<Arc<Block>>,
    mut cli_recv: UnboundedReceiver<Transaction>,
) {
    // Optimization to improve latency when the payloads are high
    let (send, mut _recv) = unbounded_channel();
    let mut cx = Context::new(config, net_send, send);
    cx.is_client_apollo_enabled = is_client_apollo_enabled;
    let myid = config.id;
    let delta = config.delta;
    let mut epoch: Height = 0;
    // A little time to boot everything up
    let begin = time::Instant::now() + Duration::from_millis(delta);
    let mut phase = Phase::End;
    let phase_end = time::sleep_until(begin);
    tokio::pin!(phase_end);
    loop {
        tokio::select! {
            pmsg_opt = net_recv.recv() => {
                // Received a protocol message
                if let None = pmsg_opt {
                    log::error!(target:"node", "Protocol message channel closed");
                    std::process::exit(0);
                }
                let (_, pmsg) = pmsg_opt.unwrap();
                let s = pmsg.to_string();
                println!("{}: Received {:?}.", myid, s);
                let time_before = time::Instant::now();
                match pmsg {
                    ProtocolMsg::Certificate(p) => {
                        if myid == cx.last_leader && phase == Phase::Propose {
                            // Check that the certificate is valid.
                            for vote in p.votes.iter() {
                                if !cx.pub_key_map.get(&vote.origin).unwrap().verify(&vote.msg, &vote.auth) {
                                    println!("[WARN] Cannot verify the certificate.")
                                }
                            }
                            let hash = if p.votes.len() == 0 { EMPTY_HASH.to_vec() } else { p.votes[0].msg.clone() };
                            if let Some(block) = cx.storage.committed_blocks_by_hash.get(&TryInto::<[u8; 32]>::try_into(hash).unwrap()) {
                                if block.header.height > cx.highest_height {
                                    cx.highest_cert = p;
                                    cx.highest_height = block.header.height;
                                }
                            }
                        }
                    },
                    ProtocolMsg::Propose(p, z) => {
                        cx.received_propose = Some(p);
                        cx.received_propose_sign = Some(z);
                    },
                    ProtocolMsg::Vote(p) => {
                        cx.received_vote.push(p);
                        if cx.received_vote.len() == (cx.num_faults + 1) as usize {
                            let certificate = Certificate {
                                votes: cx.received_vote.clone(),
                            };
                            let sign = get_sign(&cx, &certificate);
                            cx.net_send.send((cx.num_nodes, Arc::new(ProtocolMsg::VoteCert(certificate.clone(), sign.clone())))).unwrap();
                            cx.received_certificate = Some(certificate);
                            cx.received_certificate_sign = Some(sign);
                            deliver_vote_cert(&mut cx, myid);
                            phase = Phase::Commit;
                            phase_end.as_mut().reset(time::Instant::now() + Duration::from_millis(delta * 2));
                        }
                    },
                    ProtocolMsg::VoteCert(c, z) => {
                        cx.received_certificate = Some(c);
                        cx.received_certificate_sign = Some(z);
                        deliver_vote_cert(&mut cx, myid);
                        phase = Phase::Commit;
                        phase_end.as_mut().reset(time::Instant::now() + Duration::from_millis(delta * 2));
                    },
                    ProtocolMsg::DeliverPropose(sh, n, z) => {
                        if !cx.propose_share_sent && n == myid {
                            cx.net_send
                                .send((
                                    cx.num_nodes,
                                    Arc::new(ProtocolMsg::DeliverPropose(
                                        sh.clone(),
                                        myid,
                                        z.clone(),
                                    )),
                                ))
                                .unwrap();
                            cx.propose_share_sent = true;
                        }
                        cx.propose_gatherer.add_share(sh, n, cx.accumulator_pub_params_map.get(&cx.last_leader).unwrap(), cx.pub_key_map.get(&cx.last_leader).unwrap(), z);
                    }
                    ProtocolMsg::DeliverVoteCert(sh, n, z) => {
                        if !cx.vote_cert_share_sent && n == myid {
                            cx.net_send
                                .send((
                                    cx.num_nodes,
                                    Arc::new(ProtocolMsg::DeliverVoteCert(
                                        sh.clone(),
                                        myid,
                                        z.clone(),
                                    )),
                                ))
                                .unwrap();
                            cx.vote_cert_share_sent = true;
                        }
                        cx.vote_cert_gatherer.add_share(sh, n, cx.accumulator_pub_params_map.get(&cx.last_leader).unwrap(), cx.pub_key_map.get(&cx.last_leader).unwrap(), z);
                    }
                    ProtocolMsg::Reconstruct(sh, n, e) => {
                        let last = cx.reconstruct_queue[n as usize].back();
                        if last.is_none() || e >= last.unwrap().1 {
                            cx.reconstruct_queue[n as usize].push_back((sh, e));
                        }
                    }
                    ProtocolMsg::Commit(mut sh, c, z) => {
                        cx.rand_beacon_queue.get_mut(&cx.next_leader()).unwrap().append(&mut sh);
                        cx.received_commit = Some(c);
                        cx.received_commit_sign = Some(z);
                    }
                    ProtocolMsg::DeliverCommit(sh, n, z) => {
                        if !cx.commit_share_sent && n == myid {
                            cx.net_send
                                .send((
                                    cx.num_nodes,
                                    Arc::new(ProtocolMsg::DeliverCommit(
                                        sh.clone(),
                                        myid,
                                        z.clone(),
                                    )),
                                ))
                                .unwrap();
                            cx.commit_share_sent = true;
                        }
                        cx.commit_gatherer.add_share(sh, n, cx.accumulator_pub_params_map.get(&cx.next_leader()).unwrap(), cx.pub_key_map.get(&cx.next_leader()).unwrap(), z);
                        if cx.commit_gatherer.shard_num == cx.num_nodes - cx.num_faults {
                            let reconstructed_commit = commit_from_bytes(&cx.commit_gatherer.reconstruct(cx.num_nodes, cx.num_faults).unwrap());
                            let vote = Vote {
                                msg: crypto::hash::ser_and_hash(&reconstructed_commit).to_vec(),
                                origin: myid,
                                auth: cx.my_secret_key.sign(&crypto::hash::ser_and_hash(&reconstructed_commit)).unwrap(),
                            };
                            if myid != cx.next_leader() {
                                cx.net_send.send((cx.next_leader(), Arc::new(ProtocolMsg::Ack(vote)))).unwrap();
                            }
                        }
                    }
                    ProtocolMsg::Ack(v) => {
                        cx.received_ack.push(v);
                    }
                };
                let time_after = time::Instant::now();
                println!("{}: Message {:?} took {} ms.", myid, s, (time_after - time_before).as_millis());
            },
            _tx_opt = cli_recv.recv() => {
                // We received a message from the client
            },
            _ = &mut phase_end => {
                let s = phase.to_string();
                println!("{}: Phase {:?}", myid, s);
                let time_before = time::Instant::now();
                match phase {
                    Phase::Propose => {
                        let mut new_block = Block::new();
                        if cx.highest_cert.votes.len() == 0 {
                            new_block.header.prev = EMPTY_HASH;
                        } else {
                            new_block.header.prev = cx.highest_cert.votes[0].msg.clone().try_into().unwrap();
                        };
                        new_block.header.author = myid;
                        new_block.header.height = cx.highest_height + 1;
                        // TODO: Maybe add something to body?
                        let content = Content {
                            commits: cx.commits.clone(),
                            acks: cx.received_ack.clone(),
                        };
                        new_block.body.data = content;
                        cx.received_ack.clear();
                        new_block.update_hash();
                        let propose = Propose {
                            new_block: new_block,
                            certificate: cx.highest_cert.clone(),
                            epoch: epoch,
                        };
                        let sign = get_sign(&cx, &propose);
                        cx.net_send.send((cx.num_nodes, Arc::new(ProtocolMsg::Propose(propose.clone(), sign.clone())))).unwrap();
                        cx.received_propose = Some(propose);
                        cx.received_propose_sign = Some(sign);
                        deliver_propose(&mut cx, myid);
                        phase = Phase::DeliverCommit;
                        phase_end.as_mut().reset(begin + Duration::from_millis(delta * 11 * (epoch - 1) + delta * 8));
                    }
                    Phase::DeliverPropose => {
                        deliver_propose(&mut cx, myid);
                        phase = Phase::DeliverCommit;
                        phase_end.as_mut().reset(begin + Duration::from_millis(delta * 11 * (epoch - 1) + delta * 8));
                    }
                    Phase::DeliverCommit => {
                        if cx.received_commit.is_some() {
                            deliver_commit(&mut cx, myid);
                        }
                        if myid == cx.last_leader {
                            phase = Phase::End;
                            phase_end.as_mut().reset(begin + Duration::from_millis(delta * 11 * epoch));
                        } else {
                            phase = Phase::Vote;
                            phase_end.as_mut().reset(time::Instant::now() + Duration::from_millis(delta * 1));
                        }
                    }
                    Phase::Vote => {
                        let propose = Propose::from_bytes(&cx.propose_gatherer.reconstruct(cx.num_nodes, cx.num_faults).unwrap()[..]);
                        let mut block = propose.new_block;
                        block.update_hash();
                        let vote = Vote {
                            msg: block.hash.to_vec(),
                            origin: myid,
                            auth: cx.my_secret_key.sign(&block.hash).unwrap(),
                        };
                        cx.net_send.send((cx.last_leader, Arc::new(ProtocolMsg::Vote(vote)))).unwrap();
                        phase = Phase::End;
                        phase_end.as_mut().reset(begin + Duration::from_millis(delta * 11 * epoch));
                    }
                    Phase::Commit => {
                        let propose = Propose::from_bytes(&cx.propose_gatherer.reconstruct(cx.num_nodes, cx.num_faults).unwrap()[..]);
                        let new_block = Arc::new(propose.new_block);
                        cx.storage
                            .committed_blocks_by_hash
                            .insert(new_block.hash.clone(), Arc::clone(&new_block));
                        cx.storage
                            .committed_blocks_by_ht
                            .insert(new_block.header.height, Arc::clone(&new_block));
                        cx.received_propose = None;
                        cx.received_propose_sign = None;
                        cx.received_certificate = None;
                        cx.received_certificate_sign = None;
                        phase = Phase::End;
                        phase_end.as_mut().reset(begin + Duration::from_millis(delta * 11 * epoch));
                    }
                    Phase::End => {
                        let mut vals = Vec::with_capacity(cx.num_nodes as usize);
                        for i in 0..cx.num_nodes as usize {
                            let mut vec = Vec::with_capacity(cx.num_nodes as usize);
                            while !cx.reconstruct_queue[i].is_empty() && cx.reconstruct_queue[i].front().unwrap().1 < epoch {
                                cx.reconstruct_queue[i].pop_front();
                            }
                            while !cx.reconstruct_queue[i].is_empty() && cx.reconstruct_queue[i].front().unwrap().1 == epoch {
                                vec.push(cx.reconstruct_queue[i].pop_front().unwrap().0);
                            }
                            if vec.len() >= (cx.num_nodes - cx.num_faults) as usize {
                                vals.push(crypto::EVSS381::reconstruct(&vec));
                            }
                        }
                        let mut hash = [0 as u8; 32];
                        for val in vals {
                            let mut buf = Vec::new();
                            val.serialize(&mut buf).unwrap();
                            let update = crypto::hash::ser_and_hash(&buf);
                            for i in 0..32 {
                                hash[i] ^= update[i];
                            }
                        }
                        println!("Rand Beacon: {:x?}", hash);
                        cx.last_leader = cx.next_leader();
                        epoch += 1;
                        println!("{}: epoch {}. Leader is {}.", myid, epoch, cx.last_leader);
                        cx.propose_gatherer.clear();
                        cx.vote_cert_gatherer.clear();
                        cx.commit_gatherer.clear();
                        cx.received_vote.clear();
                        cx.propose_share_sent = false;
                        cx.vote_cert_share_sent = false;
                        cx.commit_share_sent = false;
                        if myid != cx.last_leader {
                            // Send the certification.
                            cx.net_send.send((cx.last_leader, Arc::new(ProtocolMsg::Certificate(cx.last_seen_block.certificate.clone())))).unwrap();
                            println!("{}: Certification sent.", myid);
                            phase = Phase::DeliverPropose;
                            phase_end.as_mut().reset(begin + Duration::from_millis(delta * 11 * (epoch - 1) + delta * 7));
                            if myid == cx.next_leader() {
                                // Shard generation
                                let rng = &mut StdRng::from_entropy();
                                for i in 0..cx.num_nodes {
                                    cx.shards[i as usize].clear();
                                }
                                cx.commits.clear();
                                for _ in 0..cx.num_nodes {
                                    let poly = crypto::EVSS381::commit(&cx.rand_beacon_parameter, crypto::F381::rand(rng), rng).unwrap();
                                    cx.commits.push(poly.get_commit());
                                    for j in 0..cx.num_nodes {
                                        cx.shards[j as usize].push_back(crypto::EVSS381::get_share(crypto::F381::from((j + 1) as u16), &cx.rand_beacon_parameter, &poly, rng).unwrap());
                                    }
                                }
                                let sign = get_sign(&cx, &cx.commits);
                                cx.rand_beacon_queue.get_mut(&myid).unwrap().append(&mut cx.shards[myid as usize].clone());
                                for i in 0..cx.num_nodes {
                                    if myid != i {
                                        cx.net_send.send((i, Arc::new(ProtocolMsg::Commit(cx.shards[i as usize].clone(), cx.commits.clone(), sign.clone())))).unwrap();
                                    }
                                }
                                cx.received_commit = Some(cx.commits.clone());
                                cx.received_commit_sign = Some(sign);
                            }
                        } else {
                            phase = Phase::Propose;
                            phase_end.as_mut().reset(time::Instant::now() + Duration::from_millis(delta * 2));
                        }
                        // Reconstruction Shards
                        for i in 0..cx.num_nodes {
                            let shard = cx.rand_beacon_queue.get_mut(&(i as Replica)).unwrap().pop_front();
                            if shard.is_some() {
                                cx.net_send.send((cx.num_nodes, Arc::new(ProtocolMsg::Reconstruct(shard.unwrap(), i, epoch)))).unwrap();
                            }
                        }
                    }
                };
                let time_after = time::Instant::now();
                println!("{}: Phase {:?} took {} ms.", myid, s, (time_after - time_before).as_millis());
            },
        }
    }
}
