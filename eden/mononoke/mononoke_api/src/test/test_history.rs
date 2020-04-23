/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use context::CoreContext;
use fbinit::FacebookInit;
use futures::stream::TryStreamExt;
use mononoke_types::DateTime;
use tests_utils::CreateCommitContext;

use crate::{ChangesetId, ChangesetSpecifier, Repo, RepoContext};

// Generates this commit graph:
//
// @ "c2"
// |
// o   "m2"
// |\
// | o "e3"
// | |
// | o "b3"
// | |
// o | "e2"
// | |
// o | "a4"
// |/
// o "c1"
// |
// o "e1"
// |
// o   "m1"
// |\
// o | "b2"
// | |
// o | "b1"
//   |
//   o "a3"
//   |
//   o "a2"
//   |
//   o "a1"
//
// Commits e1, e2 and e3 are empty (contain no file changes).
async fn init_repo(ctx: &CoreContext) -> Result<(RepoContext, HashMap<&'static str, ChangesetId>)> {
    let blob_repo = blobrepo_factory::new_memblob_empty(None)?;
    let mut changesets = HashMap::new();

    changesets.insert(
        "a1",
        CreateCommitContext::new_root(ctx, &blob_repo)
            .add_file("a", "1")
            .set_author_date(DateTime::from_timestamp(1000, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "a2",
        CreateCommitContext::new(ctx, &blob_repo, vec![changesets["a1"]])
            .add_file("a", "2")
            .add_file("dir1/a", "2")
            .set_author_date(DateTime::from_timestamp(2000, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "a3",
        CreateCommitContext::new(ctx, &blob_repo, vec![changesets["a2"]])
            .add_file("a", "3")
            .add_file("dir1/a", "3")
            .add_file("dir3/a", "3")
            .set_author_date(DateTime::from_timestamp(3000, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "b1",
        CreateCommitContext::new_root(ctx, &blob_repo)
            .add_file("b", "1")
            .add_file("dir2/b", "1")
            .set_author_date(DateTime::from_timestamp(1500, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "b2",
        CreateCommitContext::new(ctx, &blob_repo, vec![changesets["b1"]])
            .add_file("b", "2")
            .add_file("dir2/b", "2")
            .add_file("dir3/b", "2")
            .set_author_date(DateTime::from_timestamp(2500, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "m1",
        CreateCommitContext::new(ctx, &blob_repo, vec![changesets["b2"], changesets["a3"]])
            .add_file("a", "3")
            .add_file("dir1/a", "3")
            .add_file("dir3/a", "3")
            .add_file("b", "2")
            .add_file("dir2/b", "2")
            .add_file("dir3/b", "2")
            .set_author_date(DateTime::from_timestamp(4000, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "e1",
        CreateCommitContext::new(ctx, &blob_repo, vec![changesets["m1"]])
            .set_author_date(DateTime::from_timestamp(5000, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "c1",
        CreateCommitContext::new(ctx, &blob_repo, vec![changesets["e1"]])
            .add_file("c", "1")
            .add_file("dir3/c", "1")
            .set_author_date(DateTime::from_timestamp(6000, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "a4",
        CreateCommitContext::new(ctx, &blob_repo, vec![changesets["c1"]])
            .add_file("a", "4")
            .add_file("dir1/a", "4")
            .add_file("dir3/a", "4")
            .set_author_date(DateTime::from_timestamp(7000, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "e2",
        CreateCommitContext::new(ctx, &blob_repo, vec![changesets["a4"]])
            .set_author_date(DateTime::from_timestamp(8000, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "b3",
        CreateCommitContext::new(ctx, &blob_repo, vec![changesets["c1"]])
            .add_file("b", "3")
            .add_file("dir2/b", "3")
            .add_file("dir3/b", "3")
            .set_author_date(DateTime::from_timestamp(7500, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "e3",
        CreateCommitContext::new(ctx, &blob_repo, vec![changesets["b3"]])
            .set_author_date(DateTime::from_timestamp(8500, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "m2",
        CreateCommitContext::new(ctx, &blob_repo, vec![changesets["e2"], changesets["e3"]])
            .add_file("a", "4")
            .add_file("dir1/a", "4")
            .add_file("dir3/a", "4")
            .add_file("b", "3")
            .add_file("dir2/b", "3")
            .add_file("dir3/b", "3")
            .set_author_date(DateTime::from_timestamp(9000, 0)?)
            .commit()
            .await?,
    );
    changesets.insert(
        "c2",
        CreateCommitContext::new(ctx, &blob_repo, vec![changesets["m2"]])
            .add_file("c", "2")
            .add_file("dir3/c", "2")
            .set_author_date(DateTime::from_timestamp(10000, 0)?)
            .commit()
            .await?,
    );

    let repo = Repo::new_test(ctx.clone(), blob_repo).await?;
    let repo_ctx = RepoContext::new(ctx.clone(), Arc::new(repo))?;
    Ok((repo_ctx, changesets))
}

#[fbinit::compat_test]
async fn commit_path_history(fb: FacebookInit) -> Result<()> {
    let ctx = CoreContext::test_mock(fb);
    let (repo, changesets) = init_repo(&ctx).await?;

    let cs = repo
        .changeset(ChangesetSpecifier::Bonsai(changesets["c2"]))
        .await?
        .expect("changeset exists");

    // History of file "a" includes commits that modified "a".
    let a_path = cs.path("a")?;
    let a_history: Vec<_> = a_path
        .history(None)
        .await?
        .and_then(|cs| async move { Ok(cs.id()) })
        .try_collect()
        .await?;
    assert_eq!(
        a_history,
        vec![
            changesets["a4"],
            changesets["m1"],
            changesets["a3"],
            changesets["a2"],
            changesets["a1"],
        ]
    );

    // History of directory "dir2" includes commits that modified "dir2/b".
    let dir2_path = cs.path("dir2")?;
    let dir2_history: Vec<_> = dir2_path
        .history(None)
        .await?
        .and_then(|cs| async move { Ok(cs.id()) })
        .try_collect()
        .await?;
    assert_eq!(
        dir2_history,
        vec![
            changesets["b3"],
            changesets["m1"],
            changesets["b2"],
            changesets["b1"],
        ]
    );

    // History of directory "dir3" includes some commits on all branches.
    let dir3_path = cs.path("dir3")?;
    let dir3_history: Vec<_> = dir3_path
        .history(None)
        .await?
        .and_then(|cs| async move { Ok(cs.id()) })
        .try_collect()
        .await?;
    assert_eq!(
        dir3_history,
        vec![
            changesets["c2"],
            changesets["m2"],
            changesets["a4"],
            changesets["b3"],
            changesets["c1"],
            changesets["m1"],
            changesets["b2"],
            changesets["a3"],
        ]
    );

    // Root path history includes all commits except the empty ones.
    let root_path = cs.path("")?;
    let root_history: Vec<_> = root_path
        .history(None)
        .await?
        .and_then(|cs| async move { Ok(cs.id()) })
        .try_collect()
        .await?;
    assert_eq!(
        root_history,
        vec![
            changesets["c2"],
            changesets["m2"],
            changesets["a4"],
            changesets["b3"],
            changesets["c1"],
            changesets["m1"],
            changesets["b2"],
            changesets["a3"],
            changesets["b1"],
            changesets["a2"],
            changesets["a1"],
        ]
    );

    Ok(())
}

#[fbinit::compat_test]
async fn commit_history(fb: FacebookInit) -> Result<()> {
    let ctx = CoreContext::test_mock(fb);
    let (repo, changesets) = init_repo(&ctx).await?;

    let cs = repo
        .changeset(ChangesetSpecifier::Bonsai(changesets["c2"]))
        .await?
        .expect("changeset exists");

    // The commit history includes all commits, including empty ones.
    let history: Vec<_> = cs
        .history(None)
        .await
        .and_then(|cs| async move { Ok(cs.id()) })
        .try_collect()
        .await?;
    assert_eq!(
        history,
        vec![
            changesets["c2"],
            changesets["m2"],
            changesets["e2"],
            changesets["e3"],
            changesets["a4"],
            changesets["b3"],
            changesets["c1"],
            changesets["e1"],
            changesets["m1"],
            changesets["b2"],
            changesets["a3"],
            changesets["b1"],
            changesets["a2"],
            changesets["a1"],
        ]
    );

    // The commit history of an empty commit starts with itself.
    let cs = repo
        .changeset(ChangesetSpecifier::Bonsai(changesets["e1"]))
        .await?
        .expect("changeset exists");
    let history: Vec<_> = cs
        .history(None)
        .await
        .and_then(|cs| async move { Ok(cs.id()) })
        .try_collect()
        .await?;
    assert_eq!(
        history,
        vec![
            changesets["e1"],
            changesets["m1"],
            changesets["b2"],
            changesets["a3"],
            changesets["b1"],
            changesets["a2"],
            changesets["a1"],
        ]
    );

    // Setting until_timestamp omits some commits.
    let history: Vec<_> = cs
        .history(Some(2500))
        .await
        .and_then(|cs| async move { Ok(cs.id()) })
        .try_collect()
        .await?;
    assert_eq!(
        history,
        vec![
            changesets["e1"],
            changesets["m1"],
            changesets["b2"],
            changesets["a3"],
        ]
    );

    Ok(())
}