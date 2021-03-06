/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::batch::derive_fsnode_in_batch;
use crate::derive::derive_fsnode;
use anyhow::{Error, Result};
use async_trait::async_trait;
use blobrepo::BlobRepo;
use blobstore::{Blobstore, BlobstoreGetData};
use bytes::Bytes;
use context::CoreContext;
use derived_data::{BonsaiDerived, BonsaiDerivedMapping};
use futures::{
    compat::Future01CompatExt, stream as new_stream, StreamExt as NewStreamExt, TryStreamExt,
};
use futures_ext::{BoxFuture, FutureExt, StreamExt};
use futures_old::{
    stream::{self, FuturesUnordered},
    Future, Stream,
};
use mononoke_types::{
    BlobstoreBytes, BonsaiChangeset, ChangesetId, ContentId, FileType, FsnodeId, MPath,
};
use repo_blobstore::RepoBlobstore;
use std::{
    collections::HashMap,
    convert::{TryFrom, TryInto},
    iter::FromIterator,
};

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct RootFsnodeId(FsnodeId);

impl RootFsnodeId {
    pub fn fsnode_id(&self) -> &FsnodeId {
        &self.0
    }
    pub fn into_fsnode_id(self) -> FsnodeId {
        self.0
    }
}

impl TryFrom<BlobstoreBytes> for RootFsnodeId {
    type Error = Error;

    fn try_from(blob_bytes: BlobstoreBytes) -> Result<Self> {
        FsnodeId::from_bytes(&blob_bytes.into_bytes()).map(RootFsnodeId)
    }
}

impl TryFrom<BlobstoreGetData> for RootFsnodeId {
    type Error = Error;

    fn try_from(blob_get_data: BlobstoreGetData) -> Result<Self> {
        blob_get_data.into_bytes().try_into()
    }
}

impl From<RootFsnodeId> for BlobstoreBytes {
    fn from(root_fsnode_id: RootFsnodeId) -> Self {
        BlobstoreBytes::from_bytes(Bytes::copy_from_slice(root_fsnode_id.0.blake2().as_ref()))
    }
}

#[async_trait]
impl BonsaiDerived for RootFsnodeId {
    const NAME: &'static str = "fsnodes";
    type Mapping = RootFsnodeMapping;

    fn mapping(_ctx: &CoreContext, repo: &BlobRepo) -> Self::Mapping {
        RootFsnodeMapping::new(repo.blobstore().clone())
    }

    fn derive_from_parents(
        ctx: CoreContext,
        repo: BlobRepo,
        bonsai: BonsaiChangeset,
        parents: Vec<Self>,
    ) -> BoxFuture<Self, Error> {
        derive_fsnode(
            ctx,
            repo,
            parents
                .into_iter()
                .map(|root_fsnode_id| root_fsnode_id.fsnode_id().clone())
                .collect(),
            get_file_changes(&bonsai),
        )
        .map(RootFsnodeId)
        .boxify()
    }

    async fn batch_derive<'a, Iter>(
        ctx: &CoreContext,
        repo: &BlobRepo,
        csids: Iter,
    ) -> Result<HashMap<ChangesetId, Self>, Error>
    where
        Iter: IntoIterator<Item = ChangesetId> + Send,
        Iter::IntoIter: Send,
    {
        let csids = csids.into_iter().collect::<Vec<_>>();
        let derived = derive_fsnode_in_batch(ctx, repo, csids.clone()).await?;

        let mapping = Self::mapping(ctx, repo);

        new_stream::iter(derived.into_iter().map(|(cs_id, derived)| {
            let mapping = mapping.clone();
            async move {
                let derived = RootFsnodeId(derived);
                mapping
                    .put(ctx.clone(), cs_id.clone(), derived.clone())
                    .compat()
                    .await?;
                Ok((cs_id, derived))
            }
        }))
        .buffered(100)
        .try_collect::<HashMap<_, _>>()
        .await
    }
}

// TODO(mbthomas): this is copy-pasted from unodes
#[derive(Clone)]
pub struct RootFsnodeMapping {
    blobstore: RepoBlobstore,
}

impl RootFsnodeMapping {
    pub fn new(blobstore: RepoBlobstore) -> Self {
        Self { blobstore }
    }

    fn format_key(&self, cs_id: ChangesetId) -> String {
        format!("derived_root_fsnode.{}", cs_id)
    }

    fn fetch_fsnode(
        &self,
        ctx: CoreContext,
        cs_id: ChangesetId,
    ) -> impl Future<Item = Option<(ChangesetId, RootFsnodeId)>, Error = Error> {
        self.blobstore
            .get(ctx.clone(), self.format_key(cs_id))
            .and_then(|opt_blob| opt_blob.map(TryInto::try_into).transpose())
            .map(move |maybe_root_fsnode_id| {
                maybe_root_fsnode_id.map(|root_fsnode_id| (cs_id, root_fsnode_id))
            })
    }
}

impl BonsaiDerivedMapping for RootFsnodeMapping {
    type Value = RootFsnodeId;

    fn get(
        &self,
        ctx: CoreContext,
        csids: Vec<ChangesetId>,
    ) -> BoxFuture<HashMap<ChangesetId, Self::Value>, Error> {
        let gets = csids.into_iter().map(|cs_id| {
            self.fetch_fsnode(ctx.clone(), cs_id)
                .map(|maybe_root_fsnode_id| stream::iter_ok(maybe_root_fsnode_id.into_iter()))
        });
        FuturesUnordered::from_iter(gets)
            .flatten()
            .collect_to()
            .boxify()
    }

    fn put(&self, ctx: CoreContext, csid: ChangesetId, id: Self::Value) -> BoxFuture<(), Error> {
        self.blobstore.put(ctx, self.format_key(csid), id.into())
    }
}

pub(crate) fn get_file_changes(
    bcs: &BonsaiChangeset,
) -> Vec<(MPath, Option<(ContentId, FileType)>)> {
    bcs.file_changes()
        .map(|(mpath, file_change)| {
            (
                mpath.clone(),
                file_change.map(|file_change| (file_change.content_id(), file_change.file_type())),
            )
        })
        .collect()
}

#[cfg(test)]
mod test {
    use super::*;
    use blobstore::Loadable;
    use bookmarks::BookmarkName;
    use cloned::cloned;
    use fbinit::FacebookInit;
    use fixtures::{
        branch_even, branch_uneven, branch_wide, linear, many_diamonds, many_files_dirs,
        merge_even, merge_uneven, unshared_merge_even, unshared_merge_uneven,
    };
    use futures::future::Future as NewFuture;
    use manifest::Entry;
    use mercurial_types::{HgChangesetId, HgManifestId};
    use revset::AncestorsNodeStream;
    use test_utils::iterate_all_entries;
    use tokio_compat::runtime::Runtime;

    fn fetch_manifest_by_cs_id(
        ctx: CoreContext,
        repo: BlobRepo,
        hg_cs_id: HgChangesetId,
    ) -> impl Future<Item = HgManifestId, Error = Error> {
        hg_cs_id
            .load(ctx, repo.blobstore())
            .from_err()
            .map(|hg_cs| hg_cs.manifestid())
    }

    fn verify_fsnode(
        ctx: CoreContext,
        repo: BlobRepo,
        bcs_id: ChangesetId,
        hg_cs_id: HgChangesetId,
    ) -> impl Future<Item = (), Error = Error> {
        let fsnode_entries = RootFsnodeId::derive(ctx.clone(), repo.clone(), bcs_id)
            .from_err()
            .map(|root_fsnode| root_fsnode.fsnode_id().clone())
            .and_then({
                cloned!(ctx, repo);
                move |fsnode_id| {
                    iterate_all_entries(ctx, repo, Entry::Tree(fsnode_id))
                        .map(|(path, _)| path)
                        .collect()
                        .map(|mut paths| {
                            paths.sort();
                            paths
                        })
                }
            });

        let filenode_entries = fetch_manifest_by_cs_id(ctx.clone(), repo.clone(), hg_cs_id)
            .and_then({
                cloned!(ctx, repo);
                move |root_mf_id| {
                    iterate_all_entries(ctx, repo, Entry::Tree(root_mf_id))
                        .map(|(path, _)| path)
                        .collect()
                        .map(|mut paths| {
                            paths.sort();
                            paths
                        })
                }
            });

        fsnode_entries
            .join(filenode_entries)
            .map(|(fsnode_entries, filenode_entries)| {
                assert_eq!(fsnode_entries, filenode_entries);
            })
    }

    fn all_commits(
        ctx: CoreContext,
        repo: BlobRepo,
    ) -> impl Stream<Item = (ChangesetId, HgChangesetId), Error = Error> {
        let master_book = BookmarkName::new("master").unwrap();
        repo.get_bonsai_bookmark(ctx.clone(), &master_book)
            .map(move |maybe_bcs_id| {
                let bcs_id = maybe_bcs_id.unwrap();
                AncestorsNodeStream::new(ctx.clone(), &repo.get_changeset_fetcher(), bcs_id.clone())
                    .and_then(move |new_bcs_id| {
                        repo.get_hg_from_bonsai_changeset(ctx.clone(), new_bcs_id)
                            .map(move |hg_cs_id| (new_bcs_id, hg_cs_id))
                    })
            })
            .flatten_stream()
    }

    fn verify_repo<F>(fb: FacebookInit, repo: F, runtime: &mut Runtime)
    where
        F: NewFuture<Output = BlobRepo>,
    {
        let ctx = CoreContext::test_mock(fb);

        let repo = runtime.block_on_std(repo);

        runtime
            .block_on(
                all_commits(ctx.clone(), repo.clone())
                    .and_then(move |(bcs_id, hg_cs_id)| {
                        verify_fsnode(ctx.clone(), repo.clone(), bcs_id, hg_cs_id)
                    })
                    .collect(),
            )
            .unwrap();
    }

    #[fbinit::test]
    fn test_derive_data(fb: FacebookInit) {
        let mut runtime = Runtime::new().unwrap();
        verify_repo(fb, linear::getrepo(fb), &mut runtime);
        verify_repo(fb, branch_even::getrepo(fb), &mut runtime);
        verify_repo(fb, branch_uneven::getrepo(fb), &mut runtime);
        verify_repo(fb, branch_wide::getrepo(fb), &mut runtime);
        verify_repo(fb, many_diamonds::getrepo(fb), &mut runtime);
        verify_repo(fb, many_files_dirs::getrepo(fb), &mut runtime);
        verify_repo(fb, merge_even::getrepo(fb), &mut runtime);
        verify_repo(fb, merge_uneven::getrepo(fb), &mut runtime);
        verify_repo(fb, unshared_merge_even::getrepo(fb), &mut runtime);
        verify_repo(fb, unshared_merge_uneven::getrepo(fb), &mut runtime);
    }
}
