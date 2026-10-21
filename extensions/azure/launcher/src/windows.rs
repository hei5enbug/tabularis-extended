use crate::{windows_quote, LaunchError, Layout, HEAP_ARGUMENT, REMOVED_ENVIRONMENT};
use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::ptr::null;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetHandleInformation, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, GetExitCodeProcess, ResumeThread, TerminateProcess, WaitForSingleObject,
    CREATE_NO_WINDOW, CREATE_SUSPENDED, INFINITE, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
    STARTUPINFOW,
};

static CONTROL_JOB: Mutex<Option<usize>> = Mutex::new(None);
static CONTROL_STOPPED: AtomicBool = AtomicBool::new(false);

struct Handle(HANDLE);
impl Handle {
    fn new(handle: HANDLE) -> Result<Self, LaunchError> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(LaunchError::LaunchFailed)
        } else {
            Ok(Self(handle))
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0); }
    }
}

struct Child {
    process: Handle,
    thread: Handle,
    finished: bool,
}
impl Child {
    fn new(information: PROCESS_INFORMATION) -> Result<Self, LaunchError> {
        let process = match Handle::new(information.hProcess) {
            Ok(handle) => handle,
            Err(error) => { drop(Handle::new(information.hThread)); return Err(error); }
        };
        let thread = match Handle::new(information.hThread) {
            Ok(handle) => handle,
            Err(error) => {
                unsafe {
                    TerminateProcess(process.0, 1);
                    WaitForSingleObject(process.0, 5_000);
                }
                return Err(error);
            }
        };
        Ok(Self { process, thread, finished: false })
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if !self.finished {
            unsafe {
                TerminateProcess(self.process.0, 1);
                WaitForSingleObject(self.process.0, 5_000);
            }
        }
    }
}

unsafe extern "system" fn control_handler(event: u32) -> i32 {
    if !matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT) {
        return 0;
    }
    CONTROL_STOPPED.store(true, Ordering::SeqCst);
    if let Ok(job) = CONTROL_JOB.lock() {
        if let Some(handle) = *job {
            TerminateJobObject(handle as HANDLE, 130);
            return 1;
        }
    }
    0
}

struct Controller;
impl Controller {
    fn install(job: &Handle) -> Result<Self, LaunchError> {
        let mut current = CONTROL_JOB.lock().map_err(|_| LaunchError::LaunchFailed)?;
        if current.is_some() {
            return Err(LaunchError::LaunchFailed);
        }
        CONTROL_STOPPED.store(false, Ordering::SeqCst);
        *current = Some(job.0 as usize);
        if unsafe { SetConsoleCtrlHandler(Some(control_handler), 1) } == 0 {
            *current = None;
            return Err(LaunchError::LaunchFailed);
        }
        Ok(Self)
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        unsafe { SetConsoleCtrlHandler(Some(control_handler), 0); }
        if let Ok(mut job) = CONTROL_JOB.lock() {
            *job = None;
        }
    }
}

fn wide_nul(value: &OsStr) -> Result<Vec<u16>, LaunchError> {
    let mut units: Vec<u16> = value.encode_wide().collect();
    if units.contains(&0) || units.len() >= 32_767 {
        return Err(LaunchError::InvalidCommandLine);
    }
    units.push(0);
    Ok(units)
}

fn std_handle(which: u32) -> Result<HANDLE, LaunchError> {
    let handle = unsafe { GetStdHandle(which) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(LaunchError::LaunchFailed);
    }
    let mut flags = 0;
    if unsafe { GetHandleInformation(handle, &mut flags) } == 0
        || unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0
    {
        return Err(LaunchError::LaunchFailed);
    }
    Ok(handle)
}

pub fn launch(layout: &Layout) -> Result<u32, LaunchError> {
    for name in REMOVED_ENVIRONMENT {
        std::env::remove_var(name);
    }
    let application = wide_nul(layout.node.as_os_str())?;
    let directory = wide_nul(layout.root.as_os_str())?;
    let node: Vec<u16> = layout.node.as_os_str().encode_wide().collect();
    let heap: Vec<u16> = HEAP_ARGUMENT.encode_utf16().collect();
    let entry: Vec<u16> = layout.entry.as_os_str().encode_wide().collect();
    let mut command = windows_quote::command_line(&[&node, &heap, &entry])?;
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_USESTDHANDLES,
        hStdInput: std_handle(STD_INPUT_HANDLE)?,
        hStdOutput: std_handle(STD_OUTPUT_HANDLE)?,
        hStdError: std_handle(STD_ERROR_HANDLE)?,
        ..Default::default()
    };
    let job = Handle::new(unsafe { CreateJobObjectW(null(), null()) })?;
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe { SetHandleInformation(job.0, HANDLE_FLAG_INHERIT, 0) } == 0
        || unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
    {
        return Err(LaunchError::LaunchFailed);
    }
    let _controller = Controller::install(&job)?;
    let mut information = PROCESS_INFORMATION::default();
    if unsafe {
        CreateProcessW(
            application.as_ptr(), command.as_mut_ptr(), null(), null(), 1,
            CREATE_SUSPENDED | CREATE_NO_WINDOW, null(), directory.as_ptr(), &startup, &mut information,
        )
    } == 0 {
        return Err(LaunchError::LaunchFailed);
    }
    let mut child = Child::new(information)?;
    if unsafe { AssignProcessToJobObject(job.0, child.process.0) } == 0
        || CONTROL_STOPPED.load(Ordering::SeqCst)
        || unsafe { ResumeThread(child.thread.0) } == u32::MAX
    {
        return Err(LaunchError::LaunchFailed);
    }
    if unsafe { WaitForSingleObject(child.process.0, INFINITE) } != WAIT_OBJECT_0 {
        unsafe { TerminateJobObject(job.0, 1); }
        return Err(LaunchError::LaunchFailed);
    }
    let mut code = 0;
    if unsafe { GetExitCodeProcess(child.process.0, &mut code) } == 0 {
        return Err(LaunchError::LaunchFailed);
    }
    child.finished = true;
    Ok(code)
}
