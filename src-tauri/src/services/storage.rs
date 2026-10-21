use super::{
    contracts::ErrorCode,
    error::{failure, ServiceResult},
    security::opaque_id,
};
use fs2::FileExt;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub struct PrivateStore {
    root: PathBuf,
    max_file_bytes: u64,
}

impl PrivateStore {
    pub fn new(root: PathBuf, max_file_bytes: u64) -> ServiceResult<Self> {
        if root.exists()
            && fs::symlink_metadata(&root)
                .map_err(|_| failure(ErrorCode::ResourceLimit))?
                .file_type()
                .is_symlink()
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        fs::create_dir_all(&root).map_err(|_| failure(ErrorCode::ResourceLimit))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .map_err(|_| failure(ErrorCode::PermissionDenied))?;
        }
        #[cfg(windows)]
        {
            protect_windows_path(&root, true)?;
        }
        Ok(Self {
            root,
            max_file_bytes,
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn path(&self, id: &str) -> ServiceResult<PathBuf> {
        opaque_id(id)?;
        Ok(self.root.join(format!("{id}.json")))
    }
    fn open_private(&self, path: &Path, create: bool) -> ServiceResult<File> {
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        if create {
            options.create_new(true);
        }
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
            if create {
                protect_windows_path(path, false)?;
            }
        }
        Ok(file)
    }
    /// Opens (creating when missing) a private file in the store directory
    /// without locking it, for callers that hold their own long-lived lock.
    pub fn named_file(&self, name: &str) -> ServiceResult<File> {
        if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
            return Err(failure(ErrorCode::InvalidArgument));
        }
        let path = self.root.join(name);
        match self.open_private(&path, true) {
            Ok(file) => Ok(file),
            Err(_) => self.open_private(&path, false),
        }
    }
    /// Removes `*.tmp` files left behind by an interrupted write.
    pub fn remove_temporaries(&self) -> ServiceResult<()> {
        for entry in fs::read_dir(&self.root).map_err(|_| failure(ErrorCode::ResourceLimit))? {
            let path = entry.map_err(|_| failure(ErrorCode::ResourceLimit))?.path();
            if path.extension().is_some_and(|extension| extension == "tmp") {
                let _ = fs::remove_file(path);
            }
        }
        Ok(())
    }
    pub fn lock(&self) -> ServiceResult<File> {
        let path = self.root.join("store.lock");
        let file = match self.open_private(&path, true) {
            Ok(file) => file,
            Err(_) => self.open_private(&path, false)?,
        };
        file.lock_exclusive()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        Ok(file)
    }
    pub fn read<T: DeserializeOwned>(&self, id: &str) -> ServiceResult<Option<T>> {
        let path = self.path(id)?;
        if !path.exists() {
            return Ok(None);
        }
        let mut file = self.open_private(&path, false)?;
        if file
            .metadata()
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .len()
            > self.max_file_bytes
        {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(self.max_file_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?;
        if bytes.len() as u64 > self.max_file_bytes {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| failure(ErrorCode::ResourceLimit))
    }
    pub fn write<T: Serialize>(&self, id: &str, value: &T) -> ServiceResult<()> {
        let destination = self.path(id)?;
        let bytes = serde_json::to_vec(value).map_err(|_| failure(ErrorCode::ResourceLimit))?;
        if bytes.len() as u64 > self.max_file_bytes {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        let temporary = self.root.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = self.open_private(&temporary, true)?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| failure(ErrorCode::ResourceLimit))?;
            fs::rename(&temporary, &destination).map_err(|_| failure(ErrorCode::ResourceLimit))?;
            #[cfg(unix)]
            {
                File::open(&self.root)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| failure(ErrorCode::ResourceLimit))?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
    pub fn remove(&self, id: &str) -> ServiceResult<()> {
        match fs::remove_file(self.path(id)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(failure(ErrorCode::ResourceLimit)),
        }
    }
    pub fn ids(&self) -> ServiceResult<Vec<String>> {
        fs::read_dir(&self.root)
            .map_err(|_| failure(ErrorCode::ResourceLimit))?
            .filter_map(|entry| match entry {
                Ok(entry) => entry
                    .path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".json"))
                    .map(|id| Ok(id.to_owned())),
                Err(_) => Some(Err(failure(ErrorCode::ResourceLimit))),
            })
            .collect()
    }
    pub fn bytes(&self) -> ServiceResult<u64> {
        let mut total = 0u64;
        for id in self.ids()? {
            total = total.saturating_add(
                fs::symlink_metadata(self.path(&id)?)
                    .map_err(|_| failure(ErrorCode::ResourceLimit))?
                    .len(),
            );
        }
        Ok(total)
    }
}

#[cfg(windows)]
pub(crate) fn current_windows_sid() -> ServiceResult<String> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree},
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TokenUser, TOKEN_QUERY,
            TOKEN_USER,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        let result = (|| {
            let mut needed = 0;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
            if needed < std::mem::size_of::<TOKEN_USER>() as u32 || needed > 64 * 1024 {
                return Err(failure(ErrorCode::PermissionDenied));
            }
            let mut storage = vec![0u64; (needed as usize + 7) / 8];
            if GetTokenInformation(
                token,
                TokenUser,
                storage.as_mut_ptr().cast(),
                needed,
                &mut needed,
            ) == 0
            {
                return Err(failure(ErrorCode::PermissionDenied));
            }
            let user = &*storage.as_ptr().cast::<TOKEN_USER>();
            let mut sid = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
                return Err(failure(ErrorCode::PermissionDenied));
            }
            let mut length = 0;
            while length < 256 && *sid.add(length) != 0 {
                length += 1;
            }
            let result = if length == 256 {
                Err(failure(ErrorCode::PermissionDenied))
            } else {
                String::from_utf16(std::slice::from_raw_parts(sid, length))
                    .map_err(|_| failure(ErrorCode::PermissionDenied))
            };
            LocalFree(sid.cast());
            result
        })();
        CloseHandle(token);
        result
    }
}

#[cfg(windows)]
pub(crate) fn protect_windows_path(path: &Path, directory: bool) -> ServiceResult<()> {
    use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            SetFileSecurityW, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        },
    };
    if fs::symlink_metadata(path)
        .map_err(|_| failure(ErrorCode::PermissionDenied))?
        .file_attributes()
        & 0x400
        != 0
    {
        return Err(failure(ErrorCode::PermissionDenied));
    }
    let sid = current_windows_sid()?;
    let inheritance = if directory { "OICI" } else { "" };
    let descriptor: Vec<u16> = format!("D:P(A;{inheritance};FA;;;{sid})(A;{inheritance};FA;;;SY)")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let mut security = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor.as_ptr(),
            SDDL_REVISION_1,
            &mut security,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(failure(ErrorCode::PermissionDenied));
        }
        let status = SetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            security,
        );
        LocalFree(security);
        if status == 0 {
            return Err(failure(ErrorCode::PermissionDenied));
        }
    }
    Ok(())
}
