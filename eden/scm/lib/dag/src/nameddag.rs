/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! # nameddag
//!
//! Combination of IdMap and Dag.

use crate::id::Group;
use crate::idmap::IdMap;
use crate::idmap::IdMapLike;
use crate::idmap::SyncableIdMap;
use crate::segment::Dag;
use crate::segment::SyncableDag;
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// A DAG that uses names (slices) instead of ids as vertexes.
///
/// A high-level wrapper structure. Combination of [`IdMap`] and [`Dag`].
/// Maintains consistency of dag and map internally.
pub struct NamedDag {
    pub(crate) dag: Dag,
    pub(crate) map: IdMap,
}

impl NamedDag {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut map = IdMap::open(path.join("idmap"))?;
        // Take a lock so map and dag are loaded consistently.  A better (lock-free) way to ensure
        // this is to use a single "meta" file for both indexedlogs. However that requires some
        // API changes on the indexedlog side.
        let _locked = map.prepare_filesystem_sync()?;
        map.reload()?;
        let dag = Dag::open(path.join("segments"))?;
        Ok(Self { dag, map })
    }

    /// Build segments. Write to disk.
    pub fn build<F>(
        &mut self,
        parent_names_func: F,
        master_names: &[Box<[u8]>],
        non_master_names: &[Box<[u8]>],
    ) -> Result<()>
    where
        F: Fn(&[u8]) -> Result<Vec<Box<[u8]>>>,
    {
        // Already include specified nodes?
        if master_names
            .iter()
            .all(|n| is_ok_some(self.map.find_id_by_slice_with_max_group(n, Group::MASTER)))
            && non_master_names
                .iter()
                .all(|n| is_ok_some(self.map.find_id_by_slice(n)))
        {
            return Ok(());
        }

        // Take lock.
        let mut map = self.map.prepare_filesystem_sync()?;
        let mut dag = self.dag.prepare_filesystem_sync()?;

        // Build.
        build(
            &mut map,
            &mut dag,
            parent_names_func,
            master_names,
            non_master_names,
        )?;

        // Write to disk.
        map.sync()?;
        dag.sync(std::iter::once(&mut self.dag))?;
        Ok(())
    }

    /// Reload segments from disk.
    pub fn reload(&mut self) -> Result<()> {
        self.map.reload()?;
        self.dag.reload()?;
        Ok(())
    }

    // TODO: Consider implementing these:
    // - NamedSpanSet - SpanSet wrapper that only exposes "names".
    //   - Potentially, it has to implement smartset-like interfaces.
    // - On NamedDag, methods wrapping dag algorithms that uses NamedSpanSet
    //   as input and output.
    // Before those APIs, LowLevelAccess might have to be used by callsites.
}

/// Build IdMap and Segments for the given heads.
pub fn build<F>(
    map: &mut SyncableIdMap,
    dag: &mut SyncableDag,
    parent_names_func: F,
    master_heads: &[Box<[u8]>],
    non_master_heads: &[Box<[u8]>],
) -> Result<()>
where
    F: Fn(&[u8]) -> Result<Vec<Box<[u8]>>>,
{
    // Update IdMap.
    for (nodes, group) in [
        (master_heads, Group::MASTER),
        (non_master_heads, Group::NON_MASTER),
    ]
    .iter()
    {
        for node in nodes.iter() {
            map.assign_head(&node, &parent_names_func, *group)?;
        }
    }

    // Update segments.
    {
        let parent_ids_func = map.build_get_parents_by_id(&parent_names_func);
        for &group in Group::ALL.iter() {
            let id = map.next_free_id(group)?;
            if id > group.min_id() {
                dag.build_segments_persistent(id - 1, &parent_ids_func)?;
            }
        }
    }

    // XXX: Remove the hack and rebuild non-master data.
    map.need_rebuild_non_master = false;

    Ok(())
}

/// Provide low level access to dag and map.
/// Unsafe because it's possible to break consistency by writing to them.
///
/// This is currently used in Python bindings to provide an interface that is
/// consistent with `smartset.abstractsmartset`. Ideally, `smartset` provides
/// public commit hash interface, and there is no LowLevelAccess here.
pub unsafe trait LowLevelAccess {
    fn dag(&self) -> &Dag;
    fn map(&self) -> &IdMap;
}

unsafe impl LowLevelAccess for NamedDag {
    fn dag(&self) -> &Dag {
        &self.dag
    }
    fn map(&self) -> &IdMap {
        &self.map
    }
}

fn is_ok_some<T>(value: Result<Option<T>>) -> bool {
    match value {
        Ok(Some(_)) => true,
        _ => false,
    }
}