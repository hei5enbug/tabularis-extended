use crate::LaunchError;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Layout {
    pub root: PathBuf,
    pub node: PathBuf,
    pub entry: PathBuf,
}

impl Layout {
    pub fn discover() -> Result<Self, LaunchError> {
        Self::from_executable(&std::env::current_exe().map_err(|_| LaunchError::InvalidBundle)?)
    }

    pub fn from_executable(executable: &Path) -> Result<Self, LaunchError> {
        let executable = executable.canonicalize().map_err(|_| LaunchError::InvalidBundle)?;
        if !executable.is_file() {
            return Err(LaunchError::InvalidBundle);
        }
        let root = executable.parent().ok_or(LaunchError::InvalidBundle)?.to_owned();
        #[cfg(windows)]
        let node = resolve(&root, &["runtime", "node.exe"])?;
        #[cfg(not(windows))]
        let node = resolve(&root, &["runtime", "bin", "node"])?;
        #[cfg(unix)]
        validate_native_node(&node)?;
        let entry = resolve(&root, &["dist", "driver.mjs"])?;
        Ok(Self { root, node, entry })
    }
}

#[cfg(unix)]
fn validate_native_node(node: &Path) -> Result<(), LaunchError> {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let mut file = std::fs::File::open(node).map_err(|_| LaunchError::InvalidBundle)?;
    if file.metadata().map_err(|_| LaunchError::InvalidBundle)?.permissions().mode() & 0o111 == 0 {
        return Err(LaunchError::InvalidBundle);
    }
    let mut magic = [0; 4];
    file.read_exact(&mut magic).map_err(|_| LaunchError::InvalidBundle)?;
    #[cfg(target_os = "macos")]
    let valid = matches!(magic,
        [0xfe, 0xed, 0xfa, 0xce] | [0xce, 0xfa, 0xed, 0xfe]
        | [0xfe, 0xed, 0xfa, 0xcf] | [0xcf, 0xfa, 0xed, 0xfe]
        | [0xca, 0xfe, 0xba, 0xbe] | [0xbe, 0xba, 0xfe, 0xca]
        | [0xca, 0xfe, 0xba, 0xbf] | [0xbf, 0xba, 0xfe, 0xca]);
    #[cfg(not(target_os = "macos"))]
    let valid = magic == [0x7f, b'E', b'L', b'F'];
    if !valid { return Err(LaunchError::InvalidBundle); }
    Ok(())
}

fn resolve(root: &Path, parts: &[&str]) -> Result<PathBuf, LaunchError> {
    let mut path = root.to_owned();
    for (index, part) in parts.iter().enumerate() {
        path.push(part);
        let metadata = path.symlink_metadata().map_err(|_| LaunchError::InvalidBundle)?;
        if metadata.file_type().is_symlink() || reparse(&metadata) {
            return Err(LaunchError::InvalidBundle);
        }
        if index + 1 == parts.len() {
            if !metadata.is_file() {
                return Err(LaunchError::InvalidBundle);
            }
        } else if !metadata.is_dir() {
            return Err(LaunchError::InvalidBundle);
        }
    }
    let path = path.canonicalize().map_err(|_| LaunchError::InvalidBundle)?;
    if !path.starts_with(root) {
        return Err(LaunchError::InvalidBundle);
    }
    Ok(path)
}

#[cfg(windows)]
fn reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn reparse(_metadata: &std::fs::Metadata) -> bool {
    false
}
