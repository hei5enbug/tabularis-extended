use super::{register_credential, CredentialSource, NativeCore};
use crate::{
    cli::{ServiceAction, ServiceArgs},
    services::{
        backend::CancellationSignal,
        contracts::ErrorCode,
        dispatch::CallerTransport,
        error::{failure, redact_error, ServiceResult},
        local_ipc::EndpointKind,
        transport::{Mode, MAX_FRAME_BYTES},
    },
};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

pub async fn initialize_headless(kind: EndpointKind) -> ServiceResult<Arc<NativeCore>> {
    crate::service_platform::read_isolation_paths()?;
    crate::mcp::register_drivers_for_mcp().await;
    let core = NativeCore::headless(kind).await?;
    core.transport
        .set_legacy(Arc::new(crate::mcp::service_adapter::NativeLegacyMcp::new(
            Arc::downgrade(&core),
        )))?;
    Ok(core)
}
pub async fn run(args: &ServiceArgs) -> i32 {
    let signal = CancellationSignal::default();
    let control = signal.clone();
    let interrupt = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            control.cancel();
        }
    });
    let result = async {
        args.validate()
            .map_err(|_| failure(ErrorCode::InvalidArgument))?;
        let core = initialize_headless(EndpointKind::Cli).await?;
        let result = execute(&core, args, signal).await;
        let cleanup = core.shutdown().await;
        match result {
            Ok(code) => {
                cleanup?;
                Ok(code)
            }
            Err(error) => Err(error),
        }
    }
    .await;
    interrupt.abort();
    match result {
        Ok(code) => code,
        Err(error) => {
            let code = crate::services::transport::error_exit_code(&error);
            let _ = print_json(&json!({"error":redact_error(error)})).await;
            code
        }
    }
}
async fn execute(
    core: &Arc<NativeCore>,
    args: &ServiceArgs,
    signal: CancellationSignal,
) -> ServiceResult<i32> {
    let caller = core.caller(CallerTransport::Cli)?;
    if let Some(action) = &args.action {
        match action {
            ServiceAction::Approve {
                id,
                decision,
                reason,
            } => {
                core.approve(id, decision, reason.clone(), &caller)?;
                print_json(&json!({"approved":decision=="approve"})).await?;
            }
            ServiceAction::ArtifactSave { id, output } => {
                if !output.is_absolute() {
                    return Err(failure(ErrorCode::InvalidArgument));
                }
                let metadata = core
                    .artifacts
                    .save(id.clone(), caller, output.clone(), signal)
                    .await?;
                print_json(
                    &serde_json::to_value(metadata)
                        .map_err(|_| failure(ErrorCode::ResourceLimit))?,
                )
                .await?;
            }
        }
        return Ok(0);
    }
    if args.stdio {
        return stdio(core, args, signal).await;
    }
    let path = args
        .request
        .as_deref()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    let bytes = if path == "-" {
        request_bytes(
            tokio::io::stdin(),
            &signal,
            std::time::Duration::from_secs(30),
        )
        .await?
    } else {
        read_request_file(Path::new(path), &signal).await?
    };
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    let raw: Value =
        serde_json::from_slice(&bytes).map_err(|_| failure(ErrorCode::InvalidArgument))?;
    let request = core.decode(&raw)?;
    let source = if args.credential_stdin {
        Some(CredentialSource::Stdin)
    } else {
        args.credential_fd.map(CredentialSource::Descriptor)
    };
    let result = async {
        if let Some(source) = source {
            register_credential(core, &request, &caller, source, signal.clone()).await?;
        }
        core.transport
            .run_one(raw, caller.clone(), args.write, Some(signal))
            .await
    }
    .await;
    core.platform
        .runtime
        .secret_inputs
        .discard(&caller, &request.request_id);
    let response = result?;
    let code = crate::services::transport::exit_code(&response);
    print_json(&serde_json::to_value(response).map_err(|_| failure(ErrorCode::ResourceLimit))?)
        .await?;
    Ok(code)
}
pub(crate) async fn request_bytes<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    signal: &CancellationSignal,
    timeout: std::time::Duration,
) -> ServiceResult<Vec<u8>> {
    let read = async {
        let mut bytes = Vec::new();
        reader
            .take(MAX_FRAME_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| failure(ErrorCode::InvalidArgument))?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        Ok(bytes)
    };
    tokio::select! {biased;_=signal.cancelled()=>Err(failure(ErrorCode::Cancelled)),_=tokio::time::sleep(timeout.min(std::time::Duration::from_secs(30)))=>Err(failure(ErrorCode::DeadlineExceeded)),result=read=>result}
}
async fn read_request_file(path: &Path, signal: &CancellationSignal) -> ServiceResult<Vec<u8>> {
    let read = async {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|_| failure(ErrorCode::InvalidArgument))?;
        request_bytes(file, signal, std::time::Duration::from_secs(30)).await
    };
    tokio::select! {biased;_=signal.cancelled()=>Err(failure(ErrorCode::Cancelled)),_=tokio::time::sleep(std::time::Duration::from_secs(30))=>Err(failure(ErrorCode::DeadlineExceeded)),result=read=>result}
}
async fn print_json(value: &Value) -> ServiceResult<()> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| failure(ErrorCode::ResourceLimit))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(failure(ErrorCode::ResourceLimit));
    }
    bytes.push(b'\n');
    let mut stdout = tokio::io::stdout();
    stdout
        .write_all(&bytes)
        .await
        .map_err(|_| failure(ErrorCode::DriverExited))?;
    stdout
        .flush()
        .await
        .map_err(|_| failure(ErrorCode::DriverExited))
}
async fn bounded_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> ServiceResult<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|_| failure(ErrorCode::DriverExited))?;
        if available.is_empty() {
            return Ok(if line.is_empty() { None } else { Some(line) });
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if line.len().saturating_add(count) > MAX_FRAME_BYTES + 1 {
            return Err(failure(ErrorCode::ResourceLimit));
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if line.last() == Some(&b'\n') {
            return Ok(Some(line));
        }
    }
}
async fn stdio(
    core: &Arc<NativeCore>,
    args: &ServiceArgs,
    signal: CancellationSignal,
) -> ServiceResult<i32> {
    let caller = core.caller(CallerTransport::Cli)?;
    if args.credential_fd.is_none() {
        return core
            .transport
            .run_stdio(
                tokio::io::BufReader::new(tokio::io::stdin()),
                tokio::io::stdout(),
                caller,
                Mode::Cli,
                args.write,
                signal,
            )
            .await;
    }
    let fd = args
        .credential_fd
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    let id = args
        .credential_request_id
        .clone()
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))?;
    let (reader, mut writer) = tokio::io::duplex(64 * 1024);
    let producer_core = core.clone();
    let producer_caller = caller.clone();
    let producer_signal = CancellationSignal::default();
    let producer_cancel = producer_signal.clone();
    let original_signal = signal.clone();
    let producer_id = id.clone();
    let mut producer = tokio::spawn(async move {
        let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
        let mut consumed = false;
        loop {
            let line = tokio::select! {_ = producer_signal.cancelled()=>return Ok(()),_ = original_signal.cancelled()=>{producer_signal.cancel();return Ok(());},line=bounded_line(&mut stdin)=>line?};
            let Some(line) = line else {
                return Ok(());
            };
            if let Ok(raw) = serde_json::from_slice::<Value>(&line) {
                if raw["request_id"] == producer_id {
                    if consumed {
                        return Err(failure(ErrorCode::InvalidArgument));
                    }
                    let request = producer_core.decode(&raw)?;
                    register_credential(
                        &producer_core,
                        &request,
                        &producer_caller,
                        CredentialSource::Descriptor(fd),
                        producer_signal.clone(),
                    )
                    .await?;
                    consumed = true;
                }
            }
            writer
                .write_all(&line)
                .await
                .map_err(|_| failure(ErrorCode::DriverExited))?;
        }
    });
    let result = core
        .transport
        .run_stdio(
            tokio::io::BufReader::new(reader),
            tokio::io::stdout(),
            caller.clone(),
            Mode::Cli,
            args.write,
            signal,
        )
        .await;
    producer_cancel.cancel();
    let producer_result =
        match tokio::time::timeout(std::time::Duration::from_secs(1), &mut producer).await {
            Ok(result) => result.map_err(|_| failure(ErrorCode::DriverExited)),
            Err(_) => {
                producer.abort();
                Err(failure(ErrorCode::DeadlineExceeded))
            }
        };
    core.platform.runtime.secret_inputs.discard(&caller, &id);
    producer_result??;
    result
}

pub async fn run_mcp() -> i32 {
    let signal = CancellationSignal::default();
    let control = signal.clone();
    let interrupt = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            control.cancel();
        }
    });
    let result = async {
        let core = initialize_headless(EndpointKind::Mcp).await?;
        let caller = core.caller(CallerTransport::Mcp)?;
        let result = core
            .transport
            .run_stdio(
                tokio::io::BufReader::new(tokio::io::stdin()),
                tokio::io::stdout(),
                caller,
                Mode::Mcp,
                false,
                signal,
            )
            .await;
        let cleanup = core.shutdown().await;
        match result {
            Ok(code) => {
                cleanup?;
                Ok(code)
            }
            Err(error) => Err(error),
        }
    }
    .await;
    interrupt.abort();
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Service MCP failed");
            crate::services::transport::error_exit_code(&error)
        }
    }
}
