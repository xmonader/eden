/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "eden/fs/store/hg/HgDatapackStore.h"

#include <folly/Optional.h>
#include <folly/io/IOBuf.h>
#include <folly/logging/xlog.h>
#include <memory>
#include <optional>

#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Hash.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/model/TreeEntry.h"
#include "eden/fs/store/hg/HgProxyHash.h"

namespace facebook {
namespace eden {
namespace {
TreeEntryType fromRawTreeEntryType(RustTreeEntryType type) {
  switch (type) {
    case RustTreeEntryType::RegularFile:
      return TreeEntryType::REGULAR_FILE;
    case RustTreeEntryType::Tree:
      return TreeEntryType::TREE;
    case RustTreeEntryType::ExecutableFile:
      return TreeEntryType::EXECUTABLE_FILE;
    case RustTreeEntryType::Symlink:
      return TreeEntryType::SYMLINK;
  }
}

TreeEntry fromRawTreeEntry(
    RustTreeEntry entry,
    RelativePathPiece path,
    LocalStore::WriteBatch* writeBatch) {
  std::optional<uint64_t> size;
  std::optional<Hash> contentSha1;

  if (entry.size != nullptr) {
    size = *entry.size;
  }

  if (entry.content_sha1 != nullptr) {
    contentSha1 = Hash{*entry.content_sha1};
  }

  auto name = folly::StringPiece{entry.name.asByteRange()};
  auto hash = Hash{entry.hash};

  auto fullPath = path + RelativePathPiece(name);
  auto proxyHash = HgProxyHash::store(fullPath, hash, writeBatch);

  return TreeEntry{
      proxyHash, name, fromRawTreeEntryType(entry.ttype), size, contentSha1};
}

FOLLY_MAYBE_UNUSED std::unique_ptr<Tree> fromRawTree(
    const RustTree* tree,
    const Hash& edenTreeId,
    RelativePathPiece path,
    LocalStore::WriteBatch* writeBatch) {
  std::vector<TreeEntry> entries;

  for (uintptr_t i = 0; i < tree->length; i++) {
    auto entry = fromRawTreeEntry(tree->entries[i], path, writeBatch);
    entries.push_back(entry);
  }

  auto edenTree = std::make_unique<Tree>(std::move(entries), edenTreeId);
  auto serialized = LocalStore::serializeTree(edenTree.get());
  writeBatch->put(
      LocalStore::KeySpace::TreeFamily,
      edenTreeId,
      serialized.second.coalesce());
  writeBatch->flush();

  return edenTree;
}
} // namespace

std::unique_ptr<Blob> HgDatapackStore::getBlob(
    const Hash& id,
    const HgProxyHash& hgInfo) {
  auto content =
      store_.getBlob(hgInfo.path().stringPiece(), hgInfo.revHash().getBytes());
  if (content) {
    return std::make_unique<Blob>(id, *content);
  }

  return nullptr;
}

folly::Optional<folly::IOBuf> HgDatapackStore::getTree(
    const Hash& id,
    RelativePath path) {
  return store_.getTree(path.stringPiece(), id.getBytes());
}

} // namespace eden
} // namespace facebook
