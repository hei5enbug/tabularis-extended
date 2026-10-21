use super::super::{
    contracts::ErrorCode,
    error::{failure, ServiceResult},
};
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

pub(crate) async fn read_frame<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
) -> ServiceResult<T> {
    let size = stream
        .read_u32()
        .await
        .map_err(|_| failure(ErrorCode::DriverExited))? as usize;
    if size == 0 || size > MAX_FRAME_BYTES {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    let mut bytes = vec![0; size];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| failure(ErrorCode::DriverExited))?;
    serde_json::from_slice(&bytes).map_err(|_| failure(ErrorCode::InvalidArgument))
}

pub(crate) async fn write_frame<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> ServiceResult<()> {
    write_encoded(stream, &encode_frame(value)?).await
}

pub(crate) fn encode_frame<T: Serialize>(value: &T) -> ServiceResult<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    let mut frame = Vec::with_capacity(bytes.len() + 4);
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend(bytes);
    Ok(frame)
}
pub(crate) async fn write_encoded(
    stream: &mut (impl AsyncWrite + Unpin),
    frame: &[u8],
) -> ServiceResult<()> {
    stream
        .write_all(frame)
        .await
        .map_err(|_| failure(ErrorCode::DriverExited))?;
    stream
        .flush()
        .await
        .map_err(|_| failure(ErrorCode::DriverExited))
}
