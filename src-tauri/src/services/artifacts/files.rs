use super::super::{
    contracts::ErrorCode,
    error::{failure, ServiceResult},
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FileIdentity {
    pub bytes: u64,
    modified_seconds: u64,
    modified_nanos: u32,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanos: i64,
    #[cfg(windows)]
    volume: u32,
    #[cfg(windows)]
    index: u64,
}

fn private_metadata(metadata: &Metadata, directory: bool) -> ServiceResult<()> {
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(failure(ErrorCode::PermissionDenied));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(failure(ErrorCode::PermissionDenied));
        }
    }
    Ok(())
}

pub(super) fn directory(path: &Path) -> ServiceResult<()> {
    private_metadata(
        &fs::symlink_metadata(path).map_err(|_| failure(ErrorCode::PermissionDenied))?,
        true,
    )
}

pub(super) fn prepare_root(root: &Path) -> ServiceResult<PathBuf> {
    if !root.is_absolute() || root.file_name().is_none() {
        return Err(failure(ErrorCode::InvalidArgument));
    }
    let parent = root
        .parent()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
        .canonicalize()
        .map_err(|_| failure(ErrorCode::PermissionDenied))?;
    if !parent.is_dir() {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    let root = parent.join(
        root.file_name()
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?,
    );
    match fs::symlink_metadata(&root) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                if let Err(error) = fs::DirBuilder::new().mode(0o700).create(&root) {
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(failure(ErrorCode::ResourceLimit));
                    }
                }
            }
            #[cfg(not(unix))]
            {
                if let Err(error) = fs::create_dir(&root) {
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(failure(ErrorCode::ResourceLimit));
                    }
                }
            }
        }
        Err(_) => return Err(failure(ErrorCode::PermissionDenied)),
    }
    let metadata = fs::symlink_metadata(&root).map_err(|_| failure(ErrorCode::PermissionDenied))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(failure(ErrorCode::PermissionDenied));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(failure(ErrorCode::PermissionDenied));
        }
    }
    Ok(root)
}

pub(super) fn create(path: &Path) -> ServiceResult<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    let file = options
        .open(path)
        .map_err(|_| failure(ErrorCode::ResourceLimit))?;
    #[cfg(windows)]
    {
        super::super::storage::protect_windows_path(path, false)?;
    }
    private_metadata(
        &file
            .metadata()
            .map_err(|_| failure(ErrorCode::PermissionDenied))?,
        false,
    )?;
    Ok(file)
}

pub(super) fn open(path: &Path) -> ServiceResult<File> {
    let metadata = fs::symlink_metadata(path).map_err(|_| failure(ErrorCode::ResultExpired))?;
    private_metadata(&metadata, false).map_err(|_| failure(ErrorCode::ResultExpired))?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    let file = options
        .open(path)
        .map_err(|_| failure(ErrorCode::ResultExpired))?;
    private_metadata(
        &file
            .metadata()
            .map_err(|_| failure(ErrorCode::ResultExpired))?,
        false,
    )
    .map_err(|_| failure(ErrorCode::ResultExpired))?;
    if !same_object(path, &file)? {
        return Err(failure(ErrorCode::ResultExpired));
    }
    Ok(file)
}

fn same_object(path: &Path, file: &File) -> ServiceResult<bool> {
    let first = fs::symlink_metadata(path).map_err(|_| failure(ErrorCode::ResultExpired))?;
    private_metadata(&first, false).map_err(|_| failure(ErrorCode::ResultExpired))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let second = file
            .metadata()
            .map_err(|_| failure(ErrorCode::ResultExpired))?;
        Ok(first.dev() == second.dev() && first.ino() == second.ino())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let named = OpenOptions::new()
            .read(true)
            .custom_flags(0x00200000)
            .open(path)
            .map_err(|_| failure(ErrorCode::ResultExpired))?;
        private_metadata(
            &named
                .metadata()
                .map_err(|_| failure(ErrorCode::ResultExpired))?,
            false,
        )
        .map_err(|_| failure(ErrorCode::ResultExpired))?;
        Ok(windows_identity(&named)? == windows_identity(file)?)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (first, file);
        Err(failure(ErrorCode::UnsupportedOperation))
    }
}

#[cfg(windows)]
fn windows_identity(file: &File) -> ServiceResult<(u32, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0
        || information.dwFileAttributes & (0x400 | 0x10) != 0
    {
        return Err(failure(ErrorCode::ResultExpired));
    }
    Ok((
        information.dwVolumeSerialNumber,
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    ))
}

pub(super) fn identity(file: &File) -> ServiceResult<FileIdentity> {
    let metadata = file
        .metadata()
        .map_err(|_| failure(ErrorCode::ResultExpired))?;
    private_metadata(&metadata, false).map_err(|_| failure(ErrorCode::ResultExpired))?;
    let modified = metadata
        .modified()
        .map_err(|_| failure(ErrorCode::ResultExpired))?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| failure(ErrorCode::ResultExpired))?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    #[cfg(windows)]
    let (volume, index) = windows_identity(file)?;
    Ok(FileIdentity {
        bytes: metadata.len(),
        modified_seconds: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(unix)]
        changed_seconds: metadata.ctime(),
        #[cfg(unix)]
        changed_nanos: metadata.ctime_nsec(),
        #[cfg(windows)]
        volume,
        #[cfg(windows)]
        index,
    })
}

pub(super) fn verify(path: &Path, file: &File, expected: &FileIdentity) -> ServiceResult<()> {
    let named = fs::symlink_metadata(path).map_err(|_| failure(ErrorCode::ResultExpired))?;
    private_metadata(&named, false).map_err(|_| failure(ErrorCode::ResultExpired))?;
    if !same_object(path, file)? || identity(file)? != *expected {
        return Err(failure(ErrorCode::ResultExpired));
    }
    Ok(())
}

pub(super) fn read_json<T: DeserializeOwned>(path: &Path) -> ServiceResult<T> {
    let mut file = open(path)?;
    let before = identity(&file)?;
    if before.bytes > 64 * 1024 {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure(ErrorCode::ResourceLimit))?;
    verify(path, &file, &before)?;
    if bytes.len() > 64 * 1024 {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    serde_json::from_slice(&bytes).map_err(|_| failure(ErrorCode::ResourceLimit))
}

pub(super) fn sync_parent(path: &Path) -> ServiceResult<()> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

pub(super) struct CopyTemporary {
    pub path: PathBuf,
    pub file: File,
}
impl CopyTemporary {
    pub fn new(parent: &Path) -> ServiceResult<Self> {
        let path = parent.join(format!(".tabularis-artifact-{}.part", uuid::Uuid::new_v4()));
        let file = create(&path)?;
        Ok(Self { path, file })
    }
    pub fn remove(&self) -> ServiceResult<()> {
        if !same_object(&self.path, &self.file)? {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        fs::remove_file(&self.path).map_err(|_| failure(ErrorCode::ResourceLimit))
    }
}
impl Drop for CopyTemporary {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}
