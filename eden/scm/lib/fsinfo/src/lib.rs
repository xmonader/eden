/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

#[cfg(windows)]
mod windows {
    use winapi::shared::minwindef::{DWORD, MAX_PATH};
    use winapi::um::fileapi::{CreateFileW, GetVolumeInformationByHandleW, OPEN_EXISTING};
    use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use winapi::um::winnt::{
        FILE_GENERIC_READ, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, HANDLE,
    };

    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null_mut;

    const FILE_ATTRIBUTE_NORMAL: u32 = 0x02000000;

    struct WinFileHandle {
        handle: HANDLE,
    }

    impl Drop for WinFileHandle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.handle) };
        }
    }

    fn open_share<P: AsRef<Path>>(repo_root: P) -> io::Result<WinFileHandle> {
        let mut root: Vec<u16> = repo_root.as_ref().as_os_str().encode_wide().collect();
        // Need to make it 0 terminated,
        // otherwise might not get the correct
        // string
        root.push(0);

        let handle = unsafe {
            CreateFileW(
                root.as_mut_ptr(),
                FILE_GENERIC_READ,
                FILE_SHARE_DELETE | FILE_SHARE_READ | FILE_SHARE_WRITE,
                null_mut(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL as DWORD,
                null_mut(),
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(WinFileHandle { handle })
        }
    }

    pub fn get_repo_file_system<P: AsRef<Path>>(repo_root: P) -> io::Result<String> {
        let win_handle = open_share(repo_root)?;

        let mut fstype = [0u16; MAX_PATH];
        let exit_sts = unsafe {
            GetVolumeInformationByHandleW(
                win_handle.handle,
                null_mut(),
                0,
                null_mut(),
                null_mut(),
                null_mut(),
                fstype.as_mut_ptr(),
                fstype.len() as DWORD,
            )
        };

        if exit_sts == 0 {
            return Err(io::Error::last_os_error());
        }
        // Take until the first 0 byte
        let terminator = fstype.iter().position(|&x| x == 0).unwrap();
        let fstype = &fstype[0..terminator];

        Ok(String::from_utf16_lossy(&fstype))
    }
}

#[cfg(unix)]
mod unix {
    use std::ffi::CString;
    use std::io;
    use std::mem::zeroed;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub fn get_statfs<P: AsRef<Path>>(repo_root: P) -> io::Result<libc::statfs> {
        let cstr = CString::new(repo_root.as_ref().as_os_str().as_bytes())?;
        let mut fs_stat: libc::statfs = unsafe { zeroed() };
        if unsafe { libc::statfs(cstr.as_ptr(), &mut fs_stat) } == 0 {
            Ok(fs_stat)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::path::{Path, PathBuf};

    /// These filesystem types are not in libc yet
    const BTRFS_SUPER_MAGIC: i64 = 0x9123683e;
    const CIFS_SUPER_MAGIC: i64 = 0xff534d42;
    const FUSE_SUPER_MAGIC: i64 = 0x65735546;
    const XFS_SUPER_MAGIC: i64 = 0x58465342;

    fn get_type<P: AsRef<Path>>(f_type: i64, repo_root: P) -> &'static str {
        match f_type {
            BTRFS_SUPER_MAGIC => "btrfs",
            CIFS_SUPER_MAGIC => "cifs",
            FUSE_SUPER_MAGIC => {
                // Fuse system, check specifically if it is edenfs
                // by running statfs on .eden in repo_root.
                // .eden is present in all directories in an Eden mount.
                let mut repo = PathBuf::from(repo_root.as_ref());
                repo.push(".eden");
                if super::unix::get_statfs(repo_root).is_ok() {
                    "edenfs"
                } else {
                    "fuse"
                }
            }
            XFS_SUPER_MAGIC => "xfs",
            libc::CODA_SUPER_MAGIC => "coda",
            libc::CRAMFS_MAGIC => "cramfs",
            libc::EFS_SUPER_MAGIC => "efs",
            libc::EXT4_SUPER_MAGIC => "ext4",
            libc::HPFS_SUPER_MAGIC => "hpfs",
            libc::HUGETLBFS_MAGIC => "hugetlbfs",
            libc::ISOFS_SUPER_MAGIC => "isofs",
            libc::JFFS2_SUPER_MAGIC => "jffs2",
            libc::MINIX_SUPER_MAGIC | libc::MINIX_SUPER_MAGIC2 => "minix",
            libc::MINIX2_SUPER_MAGIC | libc::MINIX2_SUPER_MAGIC2 => "minix2",
            libc::NCP_SUPER_MAGIC => "ncp",
            libc::NFS_SUPER_MAGIC => "nfs",
            libc::OPENPROM_SUPER_MAGIC => "openprom",
            libc::PROC_SUPER_MAGIC => "proc",
            libc::QNX4_SUPER_MAGIC => "qnx4",
            libc::REISERFS_SUPER_MAGIC => "reiserfs",
            libc::SMB_SUPER_MAGIC => "smb",
            libc::TMPFS_MAGIC => "tmpfs",
            libc::USBDEVICE_SUPER_MAGIC => "usbdevice",
            _ => "unknown",
        }
    }

    pub fn get_repo_file_system<P: AsRef<Path>>(repo_root: P) -> io::Result<String> {
        let fs_stat = super::unix::get_statfs(repo_root.as_ref())?;
        Ok(get_type(fs_stat.f_type, repo_root.as_ref()).into())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::CStr;
    use std::io;
    use std::path::Path;

    pub fn get_repo_file_system<P: AsRef<Path>>(repo_root: P) -> io::Result<String> {
        let fs_stat = super::unix::get_statfs(repo_root)?;
        let fs = unsafe { CStr::from_ptr(fs_stat.f_fstypename.as_ptr()) };
        return Ok(fs.to_string_lossy().into());
    }
}

#[cfg(target_os = "linux")]
pub use self::linux::get_repo_file_system;
#[cfg(target_os = "macos")]
pub use self::macos::get_repo_file_system;
#[cfg(windows)]
pub use self::windows::get_repo_file_system;