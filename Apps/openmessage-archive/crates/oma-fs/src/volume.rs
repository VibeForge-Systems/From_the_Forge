//! Facts about the volume a directory sits on: free space, and what kind of
//! file system it is.
//!
//! The archive keeps a SQLite database in WAL mode and takes file locks. The
//! SQLite documentation says WAL "does not work over a network filesystem"
//! (sqlite.org/wal.html), and file locks are unreliable on virtual machine
//! shared folders, so callers refuse those volumes for archives and warn about
//! them in Settings.
//!
//! What is detected, per platform:
//!
//! - **Linux:** `statfs(2)`'s `f_type`, mapped by [`kind_from_fs_magic`], a
//!   pure function tested against a table. A magic number the table does not
//!   list is [`VolumeKind::Unknown`], never assumed local.
//! - **Windows:** only network paths: a UNC path (`\\server\share`), or a path
//!   whose `std::fs::canonicalize` form is `\\?\UNC\…`. Whether that form
//!   reveals a *mapped* network drive letter is **UNVERIFIED**
//!   (`tests/windows.rs` holds an ignored probe that records it).
//!   `GetDriveTypeW` and `GetVolumeInformationW` would answer both questions
//!   but need `unsafe`, which the workspace forbids, so a drive-letter path is
//!   [`VolumeKind::Unknown`]: neither FAT nor a mapped drive can be ruled out.
//! - **Other platforms:** always [`VolumeKind::Unknown`].

use std::io;
use std::path::Path;

/// Bytes available to this (unprivileged) user on the volume holding
/// `path`, which must exist.
///
/// Unix reads `f_bavail × f_frsize`; Windows reads
/// `GetDiskFreeSpaceExW`'s `lpFreeBytesAvailableToCaller`, which honours
/// per-user quotas (both through `fs4`). On Windows, `fs4` resolves the
/// volume root into a 261-character buffer, so a very long path may fail;
/// callers treat an error as "free space unknown", never as zero.
pub fn available_space(path: &Path) -> io::Result<u64> {
    fs4::available_space(path)
}

/// The total size in bytes of the volume holding `path`, which must exist
/// (`f_blocks × f_frsize` on Unix, `GetDiskFreeSpaceExW`'s total on
/// Windows, both through `fs4`). The same long-path caveat as
/// [`available_space`] applies; callers treat an error as "unknown".
pub fn total_space(path: &Path) -> io::Result<u64> {
    fs4::total_space(path)
}

/// A network file system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NetworkKind {
    Nfs,
    /// SMB, CIFS or SMB2 (Linux).
    Smb,
    Afs,
    Ceph,
    Coda,
    /// A Windows UNC path (`\\server\share` or `\\?\UNC\server\share`).
    Unc,
}

/// A virtual machine shared folder or a user-space file system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VirtKind {
    /// Any FUSE file system (sshfs, ntfs-3g, virtiofs and others report the
    /// same magic number).
    Fuse,
    /// 9P (WSL drives, QEMU and other VM shares).
    NineP,
    /// VirtualBox shared folders.
    VboxSf,
}

/// A FAT-family file system: no hard links, no POSIX modes, and (except
/// exFAT) a 4 GiB file size limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FatKind {
    /// FAT12/16/32 (`vfat`, `msdos`).
    Msdos,
    ExFat,
}

/// What kind of volume a path is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VolumeKind {
    /// A local file system this crate recognises (ext4, btrfs, xfs, f2fs,
    /// tmpfs, ramfs, overlayfs, ecryptfs).
    Local,
    Network(NetworkKind),
    Virtualised(VirtKind),
    Fat(FatKind),
    /// Not recognised, or not detectable on this platform. Adds nothing.
    Unknown,
}

impl VolumeKind {
    /// A short, stable, human-readable name, for messages and logs.
    pub fn label(self) -> &'static str {
        match self {
            VolumeKind::Local => "local",
            VolumeKind::Network(NetworkKind::Nfs) => "NFS",
            VolumeKind::Network(NetworkKind::Smb) => "SMB/CIFS",
            VolumeKind::Network(NetworkKind::Afs) => "AFS",
            VolumeKind::Network(NetworkKind::Ceph) => "Ceph",
            VolumeKind::Network(NetworkKind::Coda) => "Coda",
            VolumeKind::Network(NetworkKind::Unc) => "Windows network share",
            VolumeKind::Virtualised(VirtKind::Fuse) => "FUSE",
            VolumeKind::Virtualised(VirtKind::NineP) => "9P",
            VolumeKind::Virtualised(VirtKind::VboxSf) => "VirtualBox shared folder",
            VolumeKind::Fat(FatKind::Msdos) => "FAT",
            VolumeKind::Fat(FatKind::ExFat) => "exFAT",
            VolumeKind::Unknown => "unknown",
        }
    }
}

/// Linux `statfs(2)` magic numbers. Values are from the kernel's UAPI header
/// `linux/magic.h` (checked against the copy on the development machine),
/// except `vboxsf`, which defines its magic in its own driver source.
mod magic {
    // Local.
    pub const EXT2_3_4: u32 = 0xEF53;
    pub const BTRFS: u32 = 0x9123_683E;
    pub const XFS: u32 = 0x5846_5342;
    pub const F2FS: u32 = 0xF2F5_2010;
    pub const TMPFS: u32 = 0x0102_1994;
    pub const RAMFS: u32 = 0x8584_58F6;
    pub const OVERLAYFS: u32 = 0x794C_7630;
    pub const ECRYPTFS: u32 = 0xF15F;
    // Network.
    pub const NFS: u32 = 0x6969;
    pub const SMB: u32 = 0x517B;
    pub const CIFS: u32 = 0xFF53_4D42;
    pub const SMB2: u32 = 0xFE53_4D42;
    pub const AFS_SUPER: u32 = 0x5346_414F;
    pub const AFS_FS: u32 = 0x6B41_4653;
    pub const CEPH: u32 = 0x00C3_6400;
    pub const CODA: u32 = 0x7375_7245;
    // Virtualised.
    pub const FUSE: u32 = 0x6573_5546;
    pub const V9FS: u32 = 0x0102_1997;
    pub const VBOXSF: u32 = 0x786F_4256;
    // FAT family.
    pub const MSDOS: u32 = 0x4D44;
    pub const EXFAT: u32 = 0x2011_BAB0;
}

/// Map a Linux `statfs` `f_type` to a [`VolumeKind`]. Pure, so it is tested
/// against a table without mounting anything.
///
/// `f_type` is a signed word on some architectures, so a 32-bit magic such
/// as CIFS's `0xFF534D42` can arrive sign-extended. Every magic number is
/// 32 bits: the value is accepted when its upper half is all zeros or all
/// ones, and anything else is `Unknown`.
pub fn kind_from_fs_magic(f_type: u64) -> VolumeKind {
    let high = f_type >> 32;
    if high != 0 && high != 0xFFFF_FFFF {
        return VolumeKind::Unknown;
    }
    let Ok(m) = u32::try_from(f_type & 0xFFFF_FFFF) else {
        return VolumeKind::Unknown;
    };
    match m {
        magic::EXT2_3_4
        | magic::BTRFS
        | magic::XFS
        | magic::F2FS
        | magic::TMPFS
        | magic::RAMFS
        | magic::OVERLAYFS
        | magic::ECRYPTFS => VolumeKind::Local,
        magic::NFS => VolumeKind::Network(NetworkKind::Nfs),
        magic::SMB | magic::CIFS | magic::SMB2 => VolumeKind::Network(NetworkKind::Smb),
        magic::AFS_SUPER | magic::AFS_FS => VolumeKind::Network(NetworkKind::Afs),
        magic::CEPH => VolumeKind::Network(NetworkKind::Ceph),
        magic::CODA => VolumeKind::Network(NetworkKind::Coda),
        magic::FUSE => VolumeKind::Virtualised(VirtKind::Fuse),
        magic::V9FS => VolumeKind::Virtualised(VirtKind::NineP),
        magic::VBOXSF => VolumeKind::Virtualised(VirtKind::VboxSf),
        magic::MSDOS => VolumeKind::Fat(FatKind::Msdos),
        magic::EXFAT => VolumeKind::Fat(FatKind::ExFat),
        _ => VolumeKind::Unknown,
    }
}

/// The kind of volume `path` (which must exist) is on. See the module
/// documentation for what each platform can detect.
pub fn volume_kind(path: &Path) -> io::Result<VolumeKind> {
    imp::volume_kind(path)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{kind_from_fs_magic, VolumeKind};
    use std::io;
    use std::path::Path;

    pub(super) fn volume_kind(path: &Path) -> io::Result<VolumeKind> {
        let st = rustix::fs::statfs(path).map_err(io::Error::from)?;
        // `f_type` is i64 on 64-bit targets and a 32-bit word on others;
        // widening through i64 sign-extends a signed word, which
        // `kind_from_fs_magic` accepts.
        #[allow(clippy::unnecessary_cast, clippy::cast_sign_loss)]
        let raw = st.f_type as i64 as u64;
        Ok(kind_from_fs_magic(raw))
    }
}

#[cfg(windows)]
mod imp {
    use super::{NetworkKind, VolumeKind};
    use std::io;
    use std::path::{Component, Path, Prefix};

    pub(super) fn volume_kind(path: &Path) -> io::Result<VolumeKind> {
        if let Some(kind) = kind_from_prefix(path) {
            return Ok(kind);
        }
        // `canonicalize` resolves through the volume manager; for a path on
        // a network share it returns `\\?\UNC\server\share\…`. Whether a
        // mapped drive letter resolves that way is UNVERIFIED.
        let canonical = std::fs::canonicalize(path)?;
        Ok(kind_from_prefix(&canonical).unwrap_or(VolumeKind::Unknown))
    }

    /// `Some(Network)` for a UNC prefix in any of its spellings.
    pub(crate) fn kind_from_prefix(path: &Path) -> Option<VolumeKind> {
        match path.components().next() {
            Some(Component::Prefix(p)) => match p.kind() {
                Prefix::UNC(..) | Prefix::VerbatimUNC(..) => {
                    Some(VolumeKind::Network(NetworkKind::Unc))
                }
                // `\\.\UNC\server\share` parses as a device namespace.
                Prefix::DeviceNS(name) if name.eq_ignore_ascii_case("UNC") => {
                    Some(VolumeKind::Network(NetworkKind::Unc))
                }
                _ => None,
            },
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn unc_prefixes_are_network_without_touching_the_disk() {
            for p in [
                r"\\server\share\library",
                r"\\?\UNC\server\share\library",
                r"\\.\UNC\server\share\library",
            ] {
                assert_eq!(
                    kind_from_prefix(Path::new(p)),
                    Some(VolumeKind::Network(NetworkKind::Unc)),
                    "{p}"
                );
                assert_eq!(
                    volume_kind(Path::new(p)).expect("lexical"),
                    VolumeKind::Network(NetworkKind::Unc),
                    "{p}"
                );
            }
            for p in [r"C:\Users\a", r"\\?\C:\Users\a", r"\\.\PhysicalDrive0"] {
                assert_eq!(kind_from_prefix(Path::new(p)), None, "{p}");
            }
        }

        #[test]
        fn a_local_drive_is_unknown_not_local() {
            let tmp = tempfile::tempdir().expect("tempdir");
            assert_eq!(volume_kind(tmp.path()).expect("kind"), VolumeKind::Unknown);
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::VolumeKind;
    use std::io;
    use std::path::Path;

    pub(super) fn volume_kind(path: &Path) -> io::Result<VolumeKind> {
        std::fs::metadata(path)?;
        Ok(VolumeKind::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_magic_table_maps_every_listed_file_system() {
        let table: &[(u64, VolumeKind)] = &[
            (0xEF53, VolumeKind::Local),
            (0x9123_683E, VolumeKind::Local),
            (0x5846_5342, VolumeKind::Local),
            (0xF2F5_2010, VolumeKind::Local),
            (0x0102_1994, VolumeKind::Local),
            (0x8584_58F6, VolumeKind::Local),
            (0x794C_7630, VolumeKind::Local),
            (0xF15F, VolumeKind::Local),
            (0x6969, VolumeKind::Network(NetworkKind::Nfs)),
            (0x517B, VolumeKind::Network(NetworkKind::Smb)),
            (0xFF53_4D42, VolumeKind::Network(NetworkKind::Smb)),
            (0xFE53_4D42, VolumeKind::Network(NetworkKind::Smb)),
            (0x5346_414F, VolumeKind::Network(NetworkKind::Afs)),
            (0x6B41_4653, VolumeKind::Network(NetworkKind::Afs)),
            (0x00C3_6400, VolumeKind::Network(NetworkKind::Ceph)),
            (0x7375_7245, VolumeKind::Network(NetworkKind::Coda)),
            (0x6573_5546, VolumeKind::Virtualised(VirtKind::Fuse)),
            (0x0102_1997, VolumeKind::Virtualised(VirtKind::NineP)),
            (0x786F_4256, VolumeKind::Virtualised(VirtKind::VboxSf)),
            (0x4D44, VolumeKind::Fat(FatKind::Msdos)),
            (0x2011_BAB0, VolumeKind::Fat(FatKind::ExFat)),
            // Not listed: ZFS, NTFS (ntfs3), procfs, and zero.
            (0x2FC1_2FC1, VolumeKind::Unknown),
            (0x5346_544E, VolumeKind::Unknown),
            (0x9FA0, VolumeKind::Unknown),
            (0, VolumeKind::Unknown),
        ];
        for &(magic, kind) in table {
            assert_eq!(kind_from_fs_magic(magic), kind, "{magic:#x}");
        }
    }

    #[test]
    fn a_sign_extended_magic_is_accepted_and_garbage_high_bits_are_not() {
        // CIFS through a signed 32-bit f_type, widened to 64 bits.
        let cifs = i64::from(0xFF53_4D42_u32 as i32);
        #[allow(clippy::cast_sign_loss)]
        let widened = cifs as u64;
        assert_eq!(widened, 0xFFFF_FFFF_FF53_4D42);
        assert_eq!(
            kind_from_fs_magic(widened),
            VolumeKind::Network(NetworkKind::Smb)
        );
        assert_eq!(
            kind_from_fs_magic(0x0000_0001_0000_6969),
            VolumeKind::Unknown
        );
        assert_eq!(
            kind_from_fs_magic(0x1234_5678_0000_EF53),
            VolumeKind::Unknown
        );
    }

    #[test]
    fn every_kind_has_a_label() {
        for kind in [
            VolumeKind::Local,
            VolumeKind::Network(NetworkKind::Unc),
            VolumeKind::Virtualised(VirtKind::NineP),
            VolumeKind::Fat(FatKind::ExFat),
            VolumeKind::Unknown,
        ] {
            assert!(!kind.label().is_empty());
        }
    }

    #[test]
    fn available_space_reads_the_temp_volume() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let free = available_space(tmp.path()).expect("free space");
        // A writable temp directory has at least one byte available.
        assert!(free > 0);
        // Windows resolves the volume root lexically, so only Linux is
        // known to refuse a missing path.
        #[cfg(target_os = "linux")]
        assert!(available_space(&tmp.path().join("missing")).is_err());
    }

    #[test]
    fn volume_kind_of_the_temp_directory_is_not_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let kind = volume_kind(tmp.path()).expect("kind");
        // Recorded, not asserted: CI temp directories are local, but a
        // developer's may be anything.
        eprintln!("temp directory volume kind: {}", kind.label());
        assert!(volume_kind(&tmp.path().join("missing")).is_err());
    }
}
