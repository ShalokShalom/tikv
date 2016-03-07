#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rocksdb::DB;

use super::cluster::{Simulator, Cluster};
use tikv::raftserver::server::Node;
use tikv::raftserver::store::{SendCh, Transport, msg};
use tikv::proto::raft_cmdpb::*;
use tikv::proto::raft_serverpb;
use tikv::raftserver::{Result, other};
use super::util;
use super::pd::PdClient;

pub struct ChannelTransport {
    senders: HashMap<u64, SendCh>,
}

impl ChannelTransport {
    pub fn new() -> Arc<RwLock<ChannelTransport>> {
        Arc::new(RwLock::new(ChannelTransport { senders: HashMap::new() }))
    }

    pub fn get_sendch(&self, store_id: u64) -> Option<SendCh> {
        self.senders.get(&store_id).cloned()
    }
}

impl Transport for ChannelTransport {
    fn add_sendch(&mut self, store_id: u64, sender: SendCh) {
        self.senders.insert(store_id, sender);
    }

    fn remove_sendch(&mut self, store_id: u64) -> Option<SendCh> {
        self.senders.remove(&store_id)
    }

    fn send(&self, msg: raft_serverpb::RaftMessage) -> Result<()> {
        let to_store = msg.get_to_peer().get_store_id();
        match self.senders.get(&to_store) {
            None => Err(other(format!("missing sender for store {}", to_store))),
            Some(sender) => sender.send_raft_msg(msg),
        }
    }
}


pub struct NodeCluster {
    cluster_id: u64,
    trans: Arc<RwLock<ChannelTransport>>,
    pd_client: Arc<RwLock<PdClient>>,
    nodes: HashMap<u64, Node<PdClient, ChannelTransport>>,
}

impl NodeCluster {
    pub fn new(cluster_id: u64, pd_client: Arc<RwLock<PdClient>>) -> NodeCluster {
        NodeCluster {
            cluster_id: cluster_id,
            trans: ChannelTransport::new(),
            pd_client: pd_client,
            nodes: HashMap::new(),
        }
    }
}

impl Simulator for NodeCluster {
    fn run_node(&mut self, node_id: u64, engine: Arc<DB>) -> u64 {
        assert!(node_id == 0 || !self.nodes.contains_key(&node_id));

        let cfg = util::new_server_config(self.cluster_id);

        let mut node = Node::new(&cfg, self.pd_client.clone(), self.trans.clone());

        node.start(vec![engine]).unwrap();
        assert!(node_id == 0 || node_id == node.get_node_id());

        let node_id = node.get_node_id();
        self.nodes.insert(node_id, node);

        node_id
    }

    fn stop_node(&mut self, node_id: u64) {
        let node = self.nodes.remove(&node_id).unwrap();
        drop(node);
    }

    fn get_node_ids(&self) -> HashSet<u64> {
        self.nodes.keys().cloned().collect()
    }

    fn call_command(&self,
                    request: RaftCommandRequest,
                    timeout: Duration)
                    -> Option<RaftCommandResponse> {
        let store_id = request.get_header().get_peer().get_store_id();
        let sender = self.trans.read().unwrap().get_sendch(store_id).unwrap();
        msg::call_command(&sender, request, timeout).unwrap()
    }
}

pub fn new_node_cluster(id: u64, count: usize) -> Cluster<NodeCluster> {
    let pd_client = Arc::new(RwLock::new(PdClient::new()));
    Cluster::new(id,
                 count,
                 NodeCluster::new(id, pd_client.clone()),
                 pd_client)
}