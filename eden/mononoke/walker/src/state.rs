/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::graph::{EdgeType, Node, NodeData, NodeType, WrappedPath};
use crate::walk::{expand_checked_nodes, OutgoingEdge, WalkVisitor};
use chashmap::CHashMap;
use context::CoreContext;
use mercurial_types::{HgChangesetId, HgFileNodeId, HgManifestId};
use mononoke_types::{ChangesetId, ContentId, FsnodeId, MPathHash};
use phases::Phase;
use std::{cmp, collections::HashSet, hash::Hash, ops::Add};

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct StepStats {
    pub error_count: usize,
    pub num_direct: usize,
    pub num_direct_new: usize,
    pub num_expanded_new: usize,
    pub visited_of_type: usize,
}

impl Add<StepStats> for StepStats {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self {
            error_count: self.error_count + other.error_count,
            num_direct: self.num_direct + other.num_direct,
            num_direct_new: self.num_direct_new + other.num_direct_new,
            num_expanded_new: self.num_expanded_new + other.num_expanded_new,
            visited_of_type: cmp::max(self.visited_of_type, other.visited_of_type),
        }
    }
}

#[derive(Debug)]
pub struct WalkStateCHashMap {
    // TODO implement ID interning to u32 or u64 for types in more than one map
    // e.g. ChangesetId, HgChangesetId, HgFileNodeId
    include_node_types: HashSet<NodeType>,
    include_edge_types: HashSet<EdgeType>,
    visited_bcs: CHashMap<ChangesetId, ()>,
    visited_bcs_mapping: CHashMap<ChangesetId, ()>,
    visited_bcs_phase: CHashMap<ChangesetId, ()>,
    visited_file: CHashMap<ContentId, ()>,
    visited_hg_cs: CHashMap<HgChangesetId, ()>,
    visited_hg_cs_mapping: CHashMap<HgChangesetId, ()>,
    visited_hg_file_envelope: CHashMap<HgFileNodeId, ()>,
    visited_hg_filenode: CHashMap<(Option<MPathHash>, HgFileNodeId), ()>,
    visited_hg_manifest: CHashMap<(Option<MPathHash>, HgManifestId), ()>,
    visited_fsnode: CHashMap<(Option<MPathHash>, FsnodeId), ()>,
    visit_count: CHashMap<NodeType, usize>,
}

/// If the state did not have this value present, true is returned.
fn record_with_path<K>(
    visited_with_path: &CHashMap<(Option<MPathHash>, K), ()>,
    k: &(WrappedPath, K),
) -> bool
where
    K: Eq + Hash + Copy,
{
    let (path, id) = k;
    let mpathhash_opt = path.as_ref().map(|m| m.get_path_hash());
    !visited_with_path.insert((mpathhash_opt, *id), ()).is_some()
}

impl WalkStateCHashMap {
    pub fn new(
        include_node_types: HashSet<NodeType>,
        include_edge_types: HashSet<EdgeType>,
    ) -> Self {
        Self {
            include_node_types,
            include_edge_types,
            visited_bcs: CHashMap::new(),
            visited_bcs_mapping: CHashMap::new(),
            visited_bcs_phase: CHashMap::new(),
            visited_file: CHashMap::new(),
            visited_hg_cs: CHashMap::new(),
            visited_hg_cs_mapping: CHashMap::new(),
            visited_hg_file_envelope: CHashMap::new(),
            visited_hg_filenode: CHashMap::new(),
            visited_hg_manifest: CHashMap::new(),
            visited_fsnode: CHashMap::new(),
            visit_count: CHashMap::new(),
        }
    }

    /// If the set did not have this value present, true is returned.
    fn needs_visit(&self, outgoing: &OutgoingEdge) -> bool {
        let target_node: &Node = &outgoing.target;
        let k = target_node.get_type();
        &self.visit_count.upsert(k, || 1, |old| *old += 1);

        match &target_node {
            Node::BonsaiChangeset(bcs_id) => self.visited_bcs.insert(*bcs_id, ()).is_none(),
            // TODO - measure if worth tracking - the mapping is cachelib enabled.
            Node::BonsaiHgMapping(bcs_id) => {
                // Does not insert, see record_resolved_visit
                !self.visited_bcs_mapping.contains_key(bcs_id)
            }
            Node::BonsaiPhaseMapping(bcs_id) => {
                // Does not insert, as can only prune visits once data resolved, see record_resolved_visit
                !self.visited_bcs_phase.contains_key(bcs_id)
            }
            Node::HgBonsaiMapping(hg_cs_id) => {
                self.visited_hg_cs_mapping.insert(*hg_cs_id, ()).is_none()
            }
            Node::HgChangeset(hg_cs_id) => self.visited_hg_cs.insert(*hg_cs_id, ()).is_none(),
            Node::HgManifest(k) => record_with_path(&self.visited_hg_manifest, k),
            Node::HgFileNode(k) => record_with_path(&self.visited_hg_filenode, k),
            Node::HgFileEnvelope(id) => self.visited_hg_file_envelope.insert(*id, ()).is_none(),
            Node::FileContent(content_id) => self.visited_file.insert(*content_id, ()).is_none(),
            Node::Fsnode(k) => record_with_path(&self.visited_fsnode, k),
            _ => true,
        }
    }

    fn record_resolved_visit(&self, resolved: &OutgoingEdge, node_data: Option<&NodeData>) {
        match (&resolved.target, node_data) {
            (
                Node::BonsaiPhaseMapping(bcs_id),
                Some(NodeData::BonsaiPhaseMapping(Some(Phase::Public))),
            ) => {
                // Only retain visit if already public, otherwise it could mutate between walks.
                self.visited_bcs_phase.insert(*bcs_id, ());
            }
            (Node::BonsaiHgMapping(bcs_id), Some(NodeData::BonsaiHgMapping(Some(_)))) => {
                self.visited_bcs_mapping.insert(*bcs_id, ());
            }
            _ => (),
        }
    }

    fn retain_edge(&self, outgoing_edge: &OutgoingEdge) -> bool {
        // Retain if a root, or if selected
        outgoing_edge.label.incoming_type().is_none()
            || (self
                .include_node_types
                .contains(&outgoing_edge.target.get_type())
                && self.include_edge_types.contains(&outgoing_edge.label))
    }

    fn get_visit_count(&self, t: &NodeType) -> usize {
        self.visit_count.get(t).map(|v| *v).unwrap_or(0)
    }
}

impl WalkVisitor<(Node, Option<NodeData>, Option<StepStats>), ()> for WalkStateCHashMap {
    fn start_step(
        &self,
        ctx: CoreContext,
        _route: Option<&()>,
        _step: &OutgoingEdge,
    ) -> CoreContext {
        ctx
    }

    fn visit(
        &self,
        _ctx: &CoreContext,
        resolved: OutgoingEdge,
        node_data: Option<NodeData>,
        _route: Option<()>,
        mut outgoing: Vec<OutgoingEdge>,
    ) -> (
        (Node, Option<NodeData>, Option<StepStats>),
        (),
        Vec<OutgoingEdge>,
    ) {
        // Filter things we don't want to enter the WalkVisitor at all.
        outgoing.retain(|e| self.retain_edge(e));
        let num_direct = outgoing.len();

        outgoing.retain(|e| self.needs_visit(&e));
        let num_direct_new = outgoing.len();

        expand_checked_nodes(&mut outgoing);
        // Make sure we don't expand to types of node and edge not wanted
        outgoing.retain(|e| self.retain_edge(e));

        self.record_resolved_visit(&resolved, node_data.as_ref());

        // Stats
        let num_expanded_new = outgoing.len();
        let node = resolved.target;

        let (error_count, node_data) = match node_data {
            Some(NodeData::ErrorAsData(_key)) => (1, None),
            Some(d) => (0, Some(d)),
            None => (0, None),
        };
        let stats = StepStats {
            error_count,
            num_direct,
            num_direct_new,
            num_expanded_new,
            visited_of_type: self.get_visit_count(&node.get_type()),
        };

        ((node, node_data, Some(stats)), (), outgoing)
    }
}
