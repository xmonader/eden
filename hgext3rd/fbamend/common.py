# common.py - common utilities for building commands
#
# Copyright 2016 Facebook, Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

from __future__ import absolute_import

from collections import defaultdict

from hgext import rebase
from mercurial import (
    cmdutil,
    context,
    copies,
    error,
    extensions,
    lock as lockmod,
)
from mercurial.i18n import _
from mercurial.node import nullrev

inhibitmod = None

def detectinhibit():
    global inhibitmod
    try:
        inhibitmod = extensions.find('inhibit')
    except KeyError:
        pass

def deinhibit(repo, contexts):
    """Remove any inhibit markers on the given change contexts."""
    if inhibitmod:
        inhibitmod._deinhibitmarkers(repo, (ctx.node() for ctx in contexts))

def getchildrelationships(repo, revs):
    """Build a defaultdict of child relationships between all descendants of
       revs. This information will prevent us from having to repeatedly
       perform children that reconstruct these relationships each time.
    """
    cl = repo.changelog
    children = defaultdict(set)
    for rev in repo.revs('(%ld)::', revs):
        for parent in cl.parentrevs(rev):
            if parent != nullrev:
                children[parent].add(rev)
    return children

def restackonce(ui, repo, rev, rebaseopts=None, childrenonly=False,
                inhibithack=False):
    """Rebase all descendants of precursors of rev onto rev, thereby
       stabilzing any non-obsolete descendants of those precursors.
       Takes in an optional dict of options for the rebase command.
       If childrenonly is True, only rebases direct children of precursors
       of rev rather than all descendants of those precursors.

       inhibithack: temporarily, make deinhibit override inhibit transaction
       handling. useful to make things obsoleted inside a transaction.
    """
    # Get visible descendants of precusors of rev.
    allprecursors = repo.revs('allprecursors(%d)', rev)
    fmt = '%s(%%ld) - %%ld' % ('children' if childrenonly else 'descendants')
    descendants = repo.revs(fmt, allprecursors, allprecursors)

    # Nothing to do if there are no descendants.
    if not descendants:
        return

    # Overwrite source and destination, leave all other options.
    if rebaseopts is None:
        rebaseopts = {}
    rebaseopts['rev'] = descendants
    rebaseopts['dest'] = rev

    # We need to ensure that the 'operation' field in the obsmarker metadata
    # is always set to 'rebase', regardless of the current command so that
    # the restacked commits will appear as 'rebased' in smartlog.
    overrides = {}
    try:
        tweakdefaults = extensions.find('tweakdefaults')
    except KeyError:
        # No tweakdefaults extension -- skip this since there is no wrapper
        # to set the metadata.
        pass
    else:
        overrides[(tweakdefaults.globaldata,
                   tweakdefaults.createmarkersoperation)] = 'rebase'

    # Perform rebase.
    with repo.ui.configoverride(overrides, 'restack'):
        # hack: make rebase obsolete commits
        if inhibithack and inhibitmod:
            inhibitmod.deinhibittransaction = True
        rebase.rebase(ui, repo, **rebaseopts)

    # Remove any preamend bookmarks on precursors.
    _clearpreamend(repo, allprecursors)

    # Deinhibit the precursors so that they will be correctly shown as
    # obsolete. Also deinhibit their ancestors to handle the situation
    # where restackonce() is being used across several transactions
    # (such as calls to `hg next --rebase`), because each transaction
    # close will result in the ancestors being re-inhibited if they have
    # unrebased (and therefore unstable) descendants. As such, the final
    # call to restackonce() at the top of the stack should deinhibit the
    # entire stack.
    ancestors = repo.set('%ld %% %d', allprecursors, rev)
    deinhibit(repo, ancestors)
    if inhibithack and inhibitmod:
        inhibitmod.deinhibittransaction = False

def _clearpreamend(repo, revs):
    """Remove any preamend bookmarks on the given revisions."""
    # Use unfiltered repo in case the given revs are hidden. This should
    # ordinarily never happen due to the inhibit extension but it's better
    # to be resilient to this case.
    repo = repo.unfiltered()
    cl = repo.changelog
    for rev in revs:
        for bookmark in repo.nodebookmarks(cl.node(rev)):
            if bookmark.endswith('.preamend'):
                repo._bookmarks.pop(bookmark, None)

def latest(repo, rev):
    """Find the "latest version" of the given revision -- either the
       latest visible successor, or the revision itself if it has no
       visible successors.
    """
    latest = repo.revs('allsuccessors(%d)', rev).last()
    return latest if latest is not None else rev

def bookmarksupdater(repo, oldids, tr):
    """Return a callable update(newid) updating the current bookmark
    and bookmarks bound to oldid to newid.
    """
    if type(oldids) is bytes:
        oldids = [oldids]
    def updatebookmarks(newid):
        dirty = False
        for oldid in oldids:
            oldbookmarks = repo.nodebookmarks(oldid)
            if oldbookmarks:
                for b in oldbookmarks:
                    repo._bookmarks[b] = newid
                dirty = True
            if dirty:
                repo._bookmarks.recordchange(tr)
    return updatebookmarks

def rewrite(repo, old, updates, head, newbases, commitopts):
    """Return (nodeid, created) where nodeid is the identifier of the
    changeset generated by the rewrite process, and created is True if
    nodeid was actually created. If created is False, nodeid
    references a changeset existing before the rewrite call.
    """
    wlock = lock = tr = None
    try:
        wlock = repo.wlock()
        lock = repo.lock()
        tr = repo.transaction('rewrite')
        if len(old.parents()) > 1: # XXX remove this unnecessary limitation.
            raise error.Abort(_('cannot amend merge changesets'))
        base = old.p1()
        updatebookmarks = bookmarksupdater(
            repo, [old.node()] + [u.node() for u in updates], tr)

        # commit a new version of the old changeset, including the update
        # collect all files which might be affected
        files = set(old.files())
        for u in updates:
            files.update(u.files())

        # Recompute copies (avoid recording a -> b -> a)
        copied = copies.pathcopies(base, head)

        # prune files which were reverted by the updates
        def samefile(f):
            if f in head.manifest():
                a = head.filectx(f)
                if f in base.manifest():
                    b = base.filectx(f)
                    return (a.data() == b.data()
                            and a.flags() == b.flags())
                else:
                    return False
            else:
                return f not in base.manifest()
        files = [f for f in files if not samefile(f)]
        # commit version of these files as defined by head
        headmf = head.manifest()

        def filectxfn(repo, ctx, path):
            if path in headmf:
                fctx = head[path]
                flags = fctx.flags()
                mctx = context.memfilectx(repo, fctx.path(), fctx.data(),
                                          islink='l' in flags,
                                          isexec='x' in flags,
                                          copied=copied.get(path))
                return mctx
            return None

        message = cmdutil.logmessage(repo.ui, commitopts)
        if not message:
            message = old.description()

        user = commitopts.get('user') or old.user()
        # TODO: In case not date is given, we should take the old commit date
        # if we are working one one changeset or mimic the fold behavior about
        # date
        date = commitopts.get('date') or None
        extra = dict(commitopts.get('extra', old.extra()))
        extra['branch'] = head.branch()

        new = context.memctx(repo,
                             parents=newbases,
                             text=message,
                             files=files,
                             filectxfn=filectxfn,
                             user=user,
                             date=date,
                             extra=extra)

        if commitopts.get('edit'):
            new._text = cmdutil.commitforceeditor(repo, new, [])
        revcount = len(repo)
        newid = repo.commitctx(new)
        new = repo[newid]
        created = len(repo) != revcount
        updatebookmarks(newid)

        tr.close()
        return newid, created
    finally:
        lockmod.release(tr, lock, wlock)
