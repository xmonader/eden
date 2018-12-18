// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! Contains structures describing configuration of the entire repo. Those structures are
//! deserialized from TOML files from metaconfig repo

use bookmarks::Bookmark;
use errors::*;
use failure::ResultExt;
use sql::mysql_async::{FromValueError, Value, prelude::{ConvIr, FromValue}};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::str;
use toml;

/// Arguments for setting up a Manifold blobstore.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifoldArgs {
    /// Bucket of the backing Manifold blobstore to connect to
    pub bucket: String,
    /// Prefix to be prepended to all the keys. In prod it should be ""
    pub prefix: String,
}

/// Configuration of a single repository
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RepoConfig {
    /// If false, this repo config is completely ignored.
    pub enabled: bool,
    /// Defines the type of repository
    pub repotype: RepoType,
    /// How large a cache to use (in bytes) for RepoGenCache derived information
    pub generation_cache_size: usize,
    /// Numerical repo id of the repo.
    pub repoid: i32,
    /// Scuba table for logging performance of operations
    pub scuba_table: Option<String>,
    /// Parameters of how to warm up the cache
    pub cache_warmup: Option<CacheWarmupParams>,
    /// Configuration for bookmarks
    pub bookmarks: Option<Vec<BookmarkParams>>,
    /// Configuration for hooks
    pub hooks: Option<Vec<HookParams>>,
    /// Pushrebase configuration options
    pub pushrebase: PushrebaseParams,
    /// LFS configuration options
    pub lfs: LfsParams,
    /// Scribe category to log all wireproto requests with full arguments.
    /// Used for replay on shadow tier.
    pub wireproto_scribe_category: Option<String>,
    /// What percent of read request verifies that returned content matches the hash
    pub hash_validation_percentage: usize,
    /// Should this repo reject write attempts
    pub readonly: RepoReadOnly,
    /// Params for the hook manager
    pub hook_manager_params: Option<HookManagerParams>,
    /// Skiplist blobstore key (used to make revset faster)
    pub skiplist_index_blobstore_key: Option<String>,
}

impl RepoConfig {
    /// Returns a db address that is referenced in this config or None if there is none
    pub fn get_db_address(&self) -> Option<&str> {
        match self.repotype {
            RepoType::BlobRemote { ref db_address, .. } => Some(&db_address),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
/// Is the repo read-only?
pub enum RepoReadOnly {
    /// This repo is read-only and should not accept pushes or other writes
    ReadOnly,
    /// This repo should accept writes.
    ReadWrite,
}

/// Configuration of warming up the Mononoke cache. This warmup happens on startup
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CacheWarmupParams {
    /// Bookmark to warmup cache for at the startup. If not set then the cache will be cold.
    pub bookmark: Bookmark,
    /// Max number to fetch during commit warmup. If not set in the config, then set to a default
    /// value.
    pub commit_limit: usize,
}

/// Configuration for the hook manager
#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct HookManagerParams {
    /// Entry limit for the hook manager result cache
    pub entrylimit: usize,

    /// Weight limit for the hook manager result cache
    pub weightlimit: usize,
}

impl Default for HookManagerParams {
    fn default() -> Self {
        Self {
            entrylimit: 1024 * 1024,
            weightlimit: 100 * 1024 * 1024, // 100Mb
        }
    }
}

/// Configuration for a bookmark
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BookmarkParams {
    /// The bookmark
    pub bookmark: Bookmark,
    /// The hooks active for the bookmark
    pub hooks: Option<Vec<String>>,
}

/// The type of the hook
#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub enum HookType {
    /// A hook that runs on the whole changeset
    PerChangeset,
    /// A hook that runs on a file in a changeset
    PerAddedOrModifiedFile,
}

/// Hook bypass
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum HookBypass {
    /// Bypass that checks that a string is in the commit message
    CommitMessage(String),
    /// Bypass that checks that a string is in the commit message
    Pushvar {
        /// Name of the pushvar
        name: String,
        /// Value of the pushvar
        value: String,
    },
}

/// Configuration for a hook
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HookParams {
    /// The name of the hook
    pub name: String,
    /// The type of the hook
    pub hook_type: HookType,
    /// The code of the hook
    pub code: Option<String>,
    /// An optional way to bypass a hook
    pub bypass: Option<HookBypass>,
}

/// Pushrebase configuration options
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PushrebaseParams {
    /// Update dates of rebased commits
    pub rewritedates: bool,
    /// How far will we go from bookmark to find rebase root
    pub recursion_limit: usize,
}

impl Default for PushrebaseParams {
    fn default() -> Self {
        PushrebaseParams {
            rewritedates: true,
            recursion_limit: 16384, // this number is fairly arbirary
        }
    }
}

/// LFS configuration options
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LfsParams {
    /// threshold in bytes, If None, Lfs is disabled
    pub threshold: Option<u64>,
}

impl Default for LfsParams {
    fn default() -> Self {
        LfsParams { threshold: None }
    }
}

/// Remote blobstore arguments
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RemoteBlobstoreArgs {
    /// Manifold arguments
    Manifold(ManifoldArgs),
    /// Multiplexed
    Multiplexed(HashMap<BlobstoreId, RemoteBlobstoreArgs>),
}

impl From<ManifoldArgs> for RemoteBlobstoreArgs {
    fn from(manifold_args: ManifoldArgs) -> Self {
        RemoteBlobstoreArgs::Manifold(manifold_args)
    }
}

/// Id used to discriminate diffirent underlying blobstore instances
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Deserialize)]
pub struct BlobstoreId(u64);

impl BlobstoreId {
    /// Construct blobstore from integer
    pub fn new(id: u64) -> Self {
        BlobstoreId(id)
    }
}

impl From<BlobstoreId> for Value {
    fn from(id: BlobstoreId) -> Self {
        Value::UInt(id.0)
    }
}

impl ConvIr<BlobstoreId> for BlobstoreId {
    fn new(v: Value) -> std::result::Result<Self, FromValueError> {
        match v {
            Value::UInt(id) => Ok(BlobstoreId(id)),
            Value::Int(id) => Ok(BlobstoreId(id as u64)), // sqlite always produces `int`
            _ => Err(FromValueError(v)),
        }
    }
    fn commit(self) -> Self {
        self
    }
    fn rollback(self) -> Value {
        self.into()
    }
}

impl FromValue for BlobstoreId {
    type Intermediate = BlobstoreId;
}

/// Types of repositories supported
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RepoType {
    /// Blob repository with path pointing to on-disk files with data. The files are stored in a
    ///
    ///
    /// NOTE: this is read-only and for development/testing only. Production uses will break things.
    BlobFiles(PathBuf),
    /// Blob repository with path pointing to on-disk files with data. The files are stored in a
    /// RocksDb database
    BlobRocks(PathBuf),
    /// Blob repository with path pointing to the directory where a server socket is going to be.
    BlobRemote {
        /// Remote blobstores arguments
        blobstores_args: RemoteBlobstoreArgs,
        /// Identifies the SQL database to connect to.
        db_address: String,
        /// If present, the number of shards to spread filenodes across
        filenode_shards: Option<usize>,
    },
    /// Blob repository with path pointing to on-disk files with data. The files are stored in a
    /// RocksDb database, and a log-normal delay is applied to access to simulate a remote store
    /// like Manifold. Params are path, mean microseconds, stddev microseconds.
    TestBlobDelayRocks(PathBuf, u64, u64),
}

/// Configuration of a metaconfig repository
#[derive(Debug, Eq, PartialEq)]
pub struct MetaConfig {}

/// Holds configuration all configuration that was read from metaconfig repository's manifest.
#[derive(Debug, PartialEq)]
pub struct RepoConfigs {
    /// Config for the config repository
    pub metaconfig: MetaConfig,
    /// Configs for all other repositories
    pub repos: HashMap<String, RepoConfig>,
}

impl RepoConfigs {
    /// Read repo configs
    pub fn read_configs<P: AsRef<Path>>(config_path: P) -> Result<Self> {
        let repos_dir = config_path.as_ref().join("repos");
        if !repos_dir.is_dir() {
            return Err(ErrorKind::InvalidFileStructure("expected 'repos' directory".into()).into());
        }
        let mut repo_configs = HashMap::new();
        for entry in repos_dir.read_dir()? {
            let entry = entry?;
            let dir_path = entry.path();
            if dir_path.is_dir() {
                let (name, config) =
                    RepoConfigs::read_single_repo_config(&dir_path, config_path.as_ref())
                        .context(format!("while opening config for {:?} repo", dir_path))?;
                repo_configs.insert(name, config);
            }
        }

        Ok(Self {
            metaconfig: MetaConfig {},
            repos: repo_configs,
        })
    }

    fn read_single_repo_config(
        repo_config_path: &Path,
        config_root_path: &Path,
    ) -> Result<(String, RepoConfig)> {
        let reponame = repo_config_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                let e: Error = ErrorKind::InvalidFileStructure(format!(
                    "invalid repo path {:?}",
                    repo_config_path
                )).into();
                e
            })?;
        let reponame = reponame.to_string();

        let config_file = repo_config_path.join("server.toml");
        if !config_file.is_file() {
            return Err(ErrorKind::InvalidFileStructure(format!(
                "expected file server.toml in {}",
                repo_config_path.to_string_lossy()
            )).into());
        }

        fn read_file(path: &Path) -> Result<Vec<u8>> {
            let file = File::open(path).context(format!("while opening {:?}", path))?;
            let mut buf_reader = BufReader::new(file);
            let mut contents = vec![];
            buf_reader
                .read_to_end(&mut contents)
                .context(format!("while reading {:?}", path))?;
            Ok(contents)
        }

        let raw_config = toml::from_slice::<RawRepoConfig>(&read_file(&config_file)?)?;

        let hooks = raw_config.hooks.clone();
        // Easier to deal with empty vector than Option
        let hooks = hooks.unwrap_or(Vec::new());

        let mut all_hook_params = vec![];
        for raw_hook_config in hooks {
            let bypass = RepoConfigs::get_bypass(raw_hook_config.clone())?;
            let hook_params = if raw_hook_config.name.starts_with("rust:") {
                // No need to load lua code for rust hook
                HookParams {
                    name: raw_hook_config.name,
                    code: None,
                    hook_type: raw_hook_config.hook_type,
                    bypass,
                }
            } else {
                let path = raw_hook_config.path.clone();
                let path = match path {
                    Some(path) => path,
                    None => {
                        return Err(ErrorKind::MissingPath().into());
                    }
                };
                let relative_prefix = "./";
                let is_relative = path.starts_with(relative_prefix);
                let path_adjusted = if is_relative {
                    let s: String = path.chars().skip(relative_prefix.len()).collect();
                    repo_config_path.join(s)
                } else {
                    config_root_path.join(path)
                };

                let contents = read_file(&path_adjusted)
                    .context(format!("while reading hook {:?}", path_adjusted))?;
                let code = str::from_utf8(&contents)?;
                let code = code.to_string();
                HookParams {
                    name: raw_hook_config.name,
                    code: Some(code),
                    hook_type: raw_hook_config.hook_type,
                    bypass,
                }
            };

            all_hook_params.push(hook_params);
        }
        Ok((
            reponame,
            RepoConfigs::convert_conf(raw_config, all_hook_params)?,
        ))
    }

    fn get_bypass(raw_hook_config: RawHookConfig) -> Result<Option<HookBypass>> {
        let bypass_commit_message = raw_hook_config
            .bypass_commit_string
            .map(|s| HookBypass::CommitMessage(s));

        let bypass_pushvar = raw_hook_config.bypass_pushvar.and_then(|s| {
            let pushvar: Vec<_> = s.split('=').map(|val| val.to_string()).collect();
            if pushvar.len() != 2 {
                return Some(Err(ErrorKind::InvalidPushvar(s).into()));
            }
            Some(Ok((
                pushvar.get(0).unwrap().clone(),
                pushvar.get(1).unwrap().clone(),
            )))
        });
        let bypass_pushvar = match bypass_pushvar {
            Some(Err(err)) => {
                return Err(err);
            }
            Some(Ok((name, value))) => Some(HookBypass::Pushvar { name, value }),
            None => None,
        };

        if bypass_commit_message.is_some() && bypass_pushvar.is_some() {
            return Err(ErrorKind::TooManyBypassOptions(raw_hook_config.name).into());
        }
        let bypass = bypass_commit_message.or(bypass_pushvar);

        Ok(bypass)
    }

    fn convert_conf(this: RawRepoConfig, hooks: Vec<HookParams>) -> Result<RepoConfig> {
        fn get_path(config: &RawRepoConfig) -> ::std::result::Result<PathBuf, ErrorKind> {
            config.path.clone().ok_or_else(|| {
                ErrorKind::InvalidConfig(format!(
                    "No path provided for {:#?} type of repo",
                    config.repotype
                ))
            })
        }

        let repotype = match this.repotype {
            RawRepoType::Files => RepoType::BlobFiles(get_path(&this)?),
            RawRepoType::BlobRocks => RepoType::BlobRocks(get_path(&this)?),
            RawRepoType::BlobRemote => {
                let remote_blobstores = this.remote_blobstore.ok_or(ErrorKind::InvalidConfig(
                    "remote blobstores must be specified".into(),
                ))?;
                let db_address = this.db_address.ok_or(ErrorKind::InvalidConfig(
                    "xdb tier was not specified".into(),
                ))?;

                let mut blobstores = HashMap::new();
                for blobstore in remote_blobstores {
                    let args = match blobstore.blobstore_type {
                        RawBlobstoreType::Manifold => {
                            let manifold_bucket =
                                blobstore.manifold_bucket.ok_or(ErrorKind::InvalidConfig(
                                    "manifold bucket must be specified".into(),
                                ))?;
                            let manifold_args = ManifoldArgs {
                                bucket: manifold_bucket,
                                prefix: blobstore.manifold_prefix.unwrap_or("".into()),
                            };
                            RemoteBlobstoreArgs::Manifold(manifold_args)
                        }
                    };
                    if blobstores.insert(blobstore.blobstore_id, args).is_some() {
                        return Err(ErrorKind::InvalidConfig(
                            "blobstore identifiers are not unique".into(),
                        ).into());
                    }
                }

                let blobstores_args = if blobstores.len() == 1 {
                    let (_, args) = blobstores.into_iter().next().unwrap();
                    args
                } else {
                    RemoteBlobstoreArgs::Multiplexed(blobstores)
                };

                RepoType::BlobRemote {
                    blobstores_args,
                    db_address,
                    filenode_shards: this.filenode_shards,
                }
            }
            RawRepoType::TestBlobDelayRocks => RepoType::TestBlobDelayRocks(
                get_path(&this)?,
                this.delay_mean.expect("mean delay must be specified"),
                this.delay_stddev.expect("stddev delay must be specified"),
            ),
        };

        let enabled = this.enabled.unwrap_or(true);
        let generation_cache_size = this.generation_cache_size.unwrap_or(10 * 1024 * 1024);
        let repoid = this.repoid;
        let scuba_table = this.scuba_table;
        let wireproto_scribe_category = this.wireproto_scribe_category;
        let cache_warmup = this.cache_warmup.map(|cache_warmup| CacheWarmupParams {
            bookmark: Bookmark::new(cache_warmup.bookmark).expect("bookmark name must be ascii"),
            commit_limit: cache_warmup.commit_limit.unwrap_or(200000),
        });
        let hook_manager_params = this.hook_manager_params.map(|params| HookManagerParams {
            entrylimit: params.entrylimit,
            weightlimit: params.weightlimit,
        });
        let bookmarks = match this.bookmarks {
            Some(bookmarks) => Some(
                bookmarks
                    .into_iter()
                    .map(|bm| BookmarkParams {
                        bookmark: Bookmark::new(bm.name).unwrap(),
                        hooks: match bm.hooks {
                            Some(hooks) => {
                                Some(hooks.into_iter().map(|rbmh| rbmh.hook_name).collect())
                            }
                            None => None,
                        },
                    })
                    .collect(),
            ),
            None => None,
        };

        let hooks_opt;
        if hooks.len() != 0 {
            hooks_opt = Some(hooks);
        } else {
            hooks_opt = None;
        }

        let pushrebase = this.pushrebase
            .map(|raw| {
                let default = PushrebaseParams::default();
                PushrebaseParams {
                    rewritedates: raw.rewritedates.unwrap_or(default.rewritedates),
                    recursion_limit: raw.recursion_limit.unwrap_or(default.recursion_limit),
                }
            })
            .unwrap_or_default();

        let lfs = match this.lfs {
            Some(lfs_params) => LfsParams {
                threshold: lfs_params.threshold,
            },
            None => LfsParams { threshold: None },
        };

        let hash_validation_percentage = this.hash_validation_percentage.unwrap_or(0);

        let readonly = if this.readonly.unwrap_or(false) {
            RepoReadOnly::ReadOnly
        } else {
            RepoReadOnly::ReadWrite
        };

        let skiplist_index_blobstore_key = this.skiplist_index_blobstore_key;
        Ok(RepoConfig {
            enabled,
            repotype,
            generation_cache_size,
            repoid,
            scuba_table,
            cache_warmup,
            hook_manager_params,
            bookmarks,
            hooks: hooks_opt,
            pushrebase,
            lfs,
            wireproto_scribe_category,
            hash_validation_percentage,
            readonly,
            skiplist_index_blobstore_key,
        })
    }
}

#[derive(Debug, Deserialize, Clone)]
struct RawRepoConfig {
    path: Option<PathBuf>,
    repotype: RawRepoType,
    enabled: Option<bool>,
    generation_cache_size: Option<usize>,
    repoid: i32,
    db_address: Option<String>,
    filenode_shards: Option<usize>,
    scuba_table: Option<String>,
    delay_mean: Option<u64>,
    delay_stddev: Option<u64>,
    io_thread_num: Option<usize>,
    cache_warmup: Option<RawCacheWarmupConfig>,
    bookmarks: Option<Vec<RawBookmarkConfig>>,
    hooks: Option<Vec<RawHookConfig>>,
    pushrebase: Option<RawPushrebaseParams>,
    lfs: Option<RawLfsParams>,
    wireproto_scribe_category: Option<String>,
    hash_validation_percentage: Option<usize>,
    readonly: Option<bool>,
    hook_manager_params: Option<HookManagerParams>,
    skiplist_index_blobstore_key: Option<String>,
    remote_blobstore: Option<Vec<RawRemoteBlobstoreConfig>>,
}

#[derive(Debug, Deserialize, Clone)]
struct RawCacheWarmupConfig {
    bookmark: String,
    commit_limit: Option<usize>,
}

#[derive(Debug, Deserialize, Clone)]
struct RawHookManagerParams {
    entrylimit: usize,
    weightlimit: usize,
}

#[derive(Debug, Deserialize, Clone)]
struct RawBookmarkConfig {
    name: String,
    hooks: Option<Vec<RawBookmarkHook>>,
}

#[derive(Debug, Deserialize, Clone)]
struct RawBookmarkHook {
    hook_name: String,
}

#[derive(Debug, Deserialize, Clone)]
struct RawHookConfig {
    name: String,
    path: Option<String>,
    hook_type: HookType,
    bypass_commit_string: Option<String>,
    bypass_pushvar: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct RawRemoteBlobstoreConfig {
    blobstore_type: RawBlobstoreType,
    blobstore_id: BlobstoreId,
    manifold_bucket: Option<String>,
    manifold_prefix: Option<String>,
}

/// Types of repositories supported
#[derive(Clone, Debug, Deserialize)]
enum RawRepoType {
    #[serde(rename = "blob:files")] Files,
    #[serde(rename = "blob:rocks")] BlobRocks,
    #[serde(rename = "blob:remote")] BlobRemote,
    #[serde(rename = "blob:testdelay")] TestBlobDelayRocks,
}

/// Types of blobstores supported
#[derive(Clone, Debug, Deserialize)]
enum RawBlobstoreType {
    #[serde(rename = "manifold")] Manifold,
}

#[derive(Clone, Debug, Deserialize)]
struct RawPushrebaseParams {
    rewritedates: Option<bool>,
    recursion_limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawLfsParams {
    threshold: Option<u64>,
}

#[cfg(test)]
mod test {
    use super::*;

    use mercurial_types::FileType;
    use std::fs::{create_dir_all, write};
    use tempdir::TempDir;

    #[test]
    fn test_read_manifest() {
        let hook1_content = "this is hook1";
        let hook2_content = "this is hook2";
        let fbsource_content = r#"
            db_address="db_address"
            repotype="blob:remote"
            generation_cache_size=1048576
            repoid=0
            scuba_table="scuba_table"
            skiplist_index_blobstore_key="skiplist_key"
            [cache_warmup]
            bookmark="master"
            commit_limit=100
            [hook_manager_params]
            entrylimit=1234
            weightlimit=4321
            [[remote_blobstore]]
            blobstore_id=0
            blobstore_type="manifold"
            manifold_bucket="bucket"
            [[remote_blobstore]]
            blobstore_id=1
            blobstore_type="manifold"
            manifold_bucket="anotherbucket"
            manifold_prefix="someprefix"
            [[bookmarks]]
            name="master"
            [[bookmarks.hooks]]
            hook_name="hook1"
            [[bookmarks.hooks]]
            hook_name="hook2"
            [[bookmarks.hooks]]
            hook_name="rust:rusthook"
            [[hooks]]
            name="hook1"
            path="common/hooks/hook1.lua"
            hook_type="PerAddedOrModifiedFile"
            bypass_commit_string="@allow_hook1"
            [[hooks]]
            name="hook2"
            path="./hooks/hook2.lua"
            hook_type="PerChangeset"
            bypass_pushvar="pushvar=pushval"
            [[hooks]]
            name="rust:rusthook"
            hook_type="PerChangeset"
            [pushrebase]
            rewritedates = false
            recursion_limit = 1024
            [lfs]
            threshold = 1000
        "#;
        let www_content = r#"
            path="/tmp/www"
            repotype="blob:files"
            repoid=1
            scuba_table="scuba_table"
            wireproto_scribe_category="category"
        "#;

        let paths = btreemap! {
            "common/hooks/hook1.lua" => (FileType::Regular, hook1_content),
            "repos/fbsource/server.toml" => (FileType::Regular, fbsource_content),
            "repos/fbsource/hooks/hook2.lua" => (FileType::Regular, hook2_content),
            "repos/www/server.toml" => (FileType::Regular, www_content),
            "my_path/my_files" => (FileType::Regular, ""),
        };

        let tmp_dir = TempDir::new("mononoke_test_config").unwrap();

        for (path, (_, content)) in paths.clone() {
            let file_path = Path::new(path);
            let dir = file_path.parent().unwrap();
            create_dir_all(tmp_dir.path().join(dir)).unwrap();
            write(tmp_dir.path().join(file_path), content).unwrap();
        }

        let repoconfig = RepoConfigs::read_configs(tmp_dir.path()).expect("failed to read configs");

        let first_manifold_args = ManifoldArgs {
            bucket: "bucket".into(),
            prefix: "".into(),
        };
        let second_manifold_args = ManifoldArgs {
            bucket: "anotherbucket".into(),
            prefix: "someprefix".into(),
        };
        let mut blobstores = HashMap::new();
        blobstores.insert(
            BlobstoreId::new(0),
            RemoteBlobstoreArgs::Manifold(first_manifold_args),
        );
        blobstores.insert(
            BlobstoreId::new(1),
            RemoteBlobstoreArgs::Manifold(second_manifold_args),
        );
        let blobstores_args = RemoteBlobstoreArgs::Multiplexed(blobstores);

        let mut repos = HashMap::new();
        repos.insert(
            "fbsource".to_string(),
            RepoConfig {
                enabled: true,
                repotype: RepoType::BlobRemote {
                    db_address: "db_address".into(),
                    blobstores_args,
                    filenode_shards: None,
                },
                generation_cache_size: 1024 * 1024,
                repoid: 0,
                scuba_table: Some("scuba_table".to_string()),
                cache_warmup: Some(CacheWarmupParams {
                    bookmark: Bookmark::new("master").unwrap(),
                    commit_limit: 100,
                }),
                hook_manager_params: Some(HookManagerParams {
                    entrylimit: 1234,
                    weightlimit: 4321,
                }),
                bookmarks: Some(vec![
                    BookmarkParams {
                        bookmark: Bookmark::new("master").unwrap(),
                        hooks: Some(vec![
                            "hook1".to_string(),
                            "hook2".to_string(),
                            "rust:rusthook".to_string(),
                        ]),
                    },
                ]),
                hooks: Some(vec![
                    HookParams {
                        name: "hook1".to_string(),
                        code: Some("this is hook1".to_string()),
                        hook_type: HookType::PerAddedOrModifiedFile,
                        bypass: Some(HookBypass::CommitMessage("@allow_hook1".into())),
                    },
                    HookParams {
                        name: "hook2".to_string(),
                        code: Some("this is hook2".to_string()),
                        hook_type: HookType::PerChangeset,
                        bypass: Some(HookBypass::Pushvar {
                            name: "pushvar".into(),
                            value: "pushval".into(),
                        }),
                    },
                    HookParams {
                        name: "rust:rusthook".to_string(),
                        code: None,
                        hook_type: HookType::PerChangeset,
                        bypass: None,
                    },
                ]),
                pushrebase: PushrebaseParams {
                    rewritedates: false,
                    recursion_limit: 1024,
                },
                lfs: LfsParams {
                    threshold: Some(1000),
                },
                wireproto_scribe_category: None,
                hash_validation_percentage: 0,
                readonly: RepoReadOnly::ReadWrite,
                skiplist_index_blobstore_key: Some("skiplist_key".into()),
            },
        );
        repos.insert(
            "www".to_string(),
            RepoConfig {
                enabled: true,
                repotype: RepoType::BlobFiles("/tmp/www".into()),
                generation_cache_size: 10 * 1024 * 1024,
                repoid: 1,
                scuba_table: Some("scuba_table".to_string()),
                cache_warmup: None,
                hook_manager_params: None,
                bookmarks: None,
                hooks: None,
                pushrebase: Default::default(),
                lfs: Default::default(),
                wireproto_scribe_category: Some("category".to_string()),
                hash_validation_percentage: 0,
                readonly: RepoReadOnly::ReadWrite,
                skiplist_index_blobstore_key: None,
            },
        );
        assert_eq!(
            repoconfig,
            RepoConfigs {
                metaconfig: MetaConfig {},
                repos,
            }
        )
    }

    #[test]
    fn test_broken_config() {
        // Two bypasses for one hook
        let hook1_content = "this is hook1";
        let content = r#"
            path="/tmp/fbsource"
            repotype="blob:rocks"
            repoid=0
            [[bookmarks]]
            name="master"
            [[bookmarks.hooks]]
            hook_name="hook1"
            [[hooks]]
            name="hook1"
            path="common/hooks/hook1.lua"
            hook_type="PerAddedOrModifiedFile"
            bypass_commit_string="@allow_hook1"
            bypass_pushvar="var=val"
        "#;

        let paths = btreemap! {
            "common/hooks/hook1.lua" => (FileType::Regular, hook1_content),
            "repos/fbsource/server.toml" => (FileType::Regular, content),
        };

        let tmp_dir = TempDir::new("mononoke_test_config").unwrap();

        for (path, (_, content)) in paths {
            let file_path = Path::new(path);
            let dir = file_path.parent().unwrap();
            create_dir_all(tmp_dir.path().join(dir)).unwrap();
            write(tmp_dir.path().join(file_path), content).unwrap();
        }

        let res = RepoConfigs::read_configs(tmp_dir.path());
        assert!(res.is_err());

        // Incorrect bypass string
        let hook1_content = "this is hook1";
        let content = r#"
            path="/tmp/fbsource"
            repotype="blob:rocks"
            repoid=0
            [[bookmarks]]
            name="master"
            [[bookmarks.hooks]]
            hook_name="hook1"
            [[hooks]]
            name="hook1"
            path="common/hooks/hook1.lua"
            hook_type="PerAddedOrModifiedFile"
            bypass_pushvar="var"
        "#;

        let paths = btreemap! {
            "common/hooks/hook1.lua" => (FileType::Regular, hook1_content),
            "repos/fbsource/server.toml" => (FileType::Regular, content),
        };

        let tmp_dir = TempDir::new("mononoke_test_config").unwrap();

        for (path, (_, content)) in paths {
            let file_path = Path::new(path);
            let dir = file_path.parent().unwrap();
            create_dir_all(tmp_dir.path().join(dir)).unwrap();
            write(tmp_dir.path().join(file_path), content).unwrap();
        }

        let res = RepoConfigs::read_configs(tmp_dir.path());
        assert!(res.is_err());
    }
}
