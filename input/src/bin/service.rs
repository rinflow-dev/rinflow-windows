// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `linkcursor-service`: the LinkCursorInput Windows service (LocalSystem,
//! started automatically). It keeps one `linkcursor-input` running in the
//! session at the console, as LocalSystem: started when the service starts
//! and whenever another session takes the console, restarted if it exits.
//! The helper runs in a job object, so it ends with the service.
//!
//! Also a small command line for the installer and the settings page (both
//! run it as an administrator):
//! - `linkcursor-service --install` registers this executable as the
//!   service (automatic start, restarted after a failure) and starts it; an
//!   existing registration is updated. Only from Program Files: the service
//!   trusts the programs beside it.
//! - `linkcursor-service --uninstall` stops and removes it.
//! - `linkcursor-service --elevated-input on|off` is the settings switch:
//!   `on` installs the service and turns the machine-wide setting on, `off`
//!   removes the service and turns it off. The service exists only while the
//!   feature is on, so no LocalSystem process runs for a PC that left it off.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use linkcursor_input::win::{self, Handle, wide};
use linkcursor_input::{INPUT_EXE, SERVICE_NAME, log};
use std::ffi::OsString;
use std::ptr::null_mut;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
    ServiceType, SessionChangeReason,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};
use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TOKEN_ADJUST_DEFAULT,
    TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY, TokenPrimary,
    TokenSessionId,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessAsUserW, GetCurrentProcess, OpenProcessToken,
    PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};

/// A helper that lived shorter than this is restarted only after a pause.
const SHORT_LIFE: Duration = Duration::from_secs(5);

define_windows_service!(ffi_service_main, service_main);

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let command = match arguments.as_slice() {
        [flag, value] if flag == "--elevated-input" => Some(set_elevated_input(value == "on")),
        [flag] if flag == "--install" => Some(install().map_err(|error| error.to_string())),
        [flag] if flag == "--uninstall" => Some(uninstall().map_err(|error| error.to_string())),
        _ => None,
    };
    if let Some(result) = command {
        match result {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("linkcursor-service: {error}");
                std::process::exit(1);
            }
        }
    }
    log::init("service");
    if let Err(error) = service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        log!("not started as a service: {error}");
        eprintln!(
            "linkcursor-service runs as the {SERVICE_NAME} Windows service; \
             or: linkcursor-service --elevated-input on|off"
        );
    }
}

/// The settings switch: the service and the setting together. On, the
/// service is installed first, so a failure leaves the setting as it was;
/// off, it is removed first, so the helper stops before the setting changes.
fn set_elevated_input(on: bool) -> Result<(), String> {
    if on {
        install().map_err(|error| error.to_string())?;
        win::set_elevated_input_setting(true).map_err(|error| error.to_string())
    } else {
        let removed = uninstall().map_err(|error| error.to_string());
        win::set_elevated_input_setting(false).map_err(|error| error.to_string())?;
        removed
    }
}

/// Registers this executable as the service, or updates the registration,
/// and starts it.
fn install() -> windows_service::Result<()> {
    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
        ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let executable = std::env::current_exe().map_err(windows_service::Error::Winapi)?;
    // LocalSystem would run whatever replaces this file: only a folder
    // ordinary users cannot write to will do.
    if !executable.parent().is_some_and(win::in_program_files) {
        return Err(windows_service::Error::Winapi(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is not in Program Files; the input service installs only from there",
                executable.display()
            ),
        )));
    }
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;
    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: linkcursor_input::SERVICE_DISPLAY_NAME.into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: executable,
        launch_arguments: Vec::new(),
        dependencies: Vec::new(),
        // LocalSystem.
        account_name: None,
        account_password: None,
    };
    let access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS;
    let service = match manager.open_service(SERVICE_NAME, access) {
        Ok(service) => {
            service.change_config(&info)?;
            service
        }
        Err(_) => manager.create_service(&info, access)?,
    };
    service.set_description(
        "Lets devices that control this PC through LinkCursor operate windows of elevated \
         programs, UAC prompts and the lock screen.",
    )?;
    let restart = ServiceAction {
        action_type: ServiceActionType::Restart,
        delay: Duration::from_secs(1),
    };
    service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 60 * 60)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart.clone(), restart.clone(), restart]),
    })?;
    if service.query_status()?.current_state != ServiceState::Running {
        service.start::<&str>(&[])?;
    }
    Ok(())
}

/// Stops the service and removes its registration.
fn uninstall() -> windows_service::Result<()> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let Ok(service) = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
    ) else {
        // Not installed: nothing to do.
        return Ok(());
    };
    if service.query_status()?.current_state != ServiceState::Stopped {
        let _ = service.stop();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline
            && service.query_status()?.current_state != ServiceState::Stopped
        {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    service.delete()
}

enum Event {
    Stop,
    /// `renew`: someone signed in or out, so the helper starts afresh.
    SessionChanged {
        renew: bool,
    },
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run() {
        log!("service failed: {error}");
    }
}

fn run() -> windows_service::Result<()> {
    let (events, received) = mpsc::channel();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = events.send(Event::Stop);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::SessionChange(change) => {
            // Someone signed in or out: the session may keep its number, but
            // the helper's pipe is for the user signed in, so a new helper
            // starts.
            let renew = matches!(
                change.reason,
                SessionChangeReason::SessionLogon | SessionChangeReason::SessionLogoff
            );
            let _ = events.send(Event::SessionChanged { renew });
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(SERVICE_NAME, handler)?;
    let set_state = |state, accept| {
        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })
    };
    set_state(
        ServiceState::Running,
        ServiceControlAccept::STOP
            | ServiceControlAccept::SHUTDOWN
            | ServiceControlAccept::SESSION_CHANGE,
    )?;
    log!("running");

    let mut supervisor = Supervisor::new();
    loop {
        supervisor.check();
        match received.recv_timeout(Duration::from_secs(1)) {
            Ok(Event::Stop) => break,
            Ok(Event::SessionChanged { renew }) => {
                log!("session change (sign-in or sign-out: {renew})");
                if renew {
                    supervisor.renew();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(supervisor);
    log!("stopped");
    set_state(ServiceState::Stopped, ServiceControlAccept::empty())?;
    Ok(())
}

struct Helper {
    process: Handle,
    session: u32,
    started: Instant,
}

/// Keeps one helper in the console session.
struct Supervisor {
    /// Ends every helper with the service (or when the service dies).
    job: Option<Handle>,
    helper: Option<Helper>,
    /// No new helper before this, after one that exited quickly.
    not_before: Option<Instant>,
}

impl Supervisor {
    fn new() -> Self {
        Self {
            job: kill_on_close_job(),
            helper: None,
            not_before: None,
        }
    }

    /// Ends the helper; the next check starts a new one, without waiting.
    fn renew(&mut self) {
        if let Some(helper) = self.helper.take() {
            // SAFETY: the helper's process handle, still open.
            unsafe { TerminateProcess(helper.process.0, 0) };
        }
        self.not_before = None;
    }

    fn check(&mut self) {
        // SAFETY: no arguments.
        let console = unsafe { WTSGetActiveConsoleSessionId() };
        if let Some(helper) = &self.helper {
            // SAFETY: the helper's process handle, still open.
            let exited = unsafe { WaitForSingleObject(helper.process.0, 0) } == WAIT_OBJECT_0;
            if exited {
                log!("helper in session {} exited", helper.session);
                if helper.started.elapsed() < SHORT_LIFE {
                    self.not_before = Some(Instant::now() + SHORT_LIFE);
                }
                self.helper = None;
            } else if helper.session != console {
                log!("console moved to session {console}");
                // SAFETY: the helper's process handle, still open.
                unsafe { TerminateProcess(helper.process.0, 0) };
                self.helper = None;
            }
        }
        // No session at the console (between logons, for a moment).
        if self.helper.is_some() || console == u32::MAX {
            return;
        }
        if self.not_before.is_some_and(|time| Instant::now() < time) {
            return;
        }
        self.not_before = None;
        match self.launch(console) {
            Ok(helper) => {
                log!("helper started in session {console}");
                self.helper = Some(helper);
            }
            Err(error) => {
                log!("cannot start the helper in session {console}: {error}");
                self.not_before = Some(Instant::now() + SHORT_LIFE);
            }
        }
    }

    /// Starts the helper in `session`, as LocalSystem: this service's own
    /// token, moved to that session.
    fn launch(&self, session: u32) -> std::io::Result<Helper> {
        let path = win::own_dir()
            .ok_or_else(|| std::io::Error::other("no own folder"))?
            .join(INPUT_EXE);
        let mut own: HANDLE = null_mut();
        // SAFETY: this process's token, closed by `Handle`.
        if unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE
                    | TOKEN_QUERY
                    | TOKEN_ASSIGN_PRIMARY
                    | TOKEN_ADJUST_SESSIONID
                    | TOKEN_ADJUST_DEFAULT,
                &mut own,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let own = Handle(own);
        let mut token: HANDLE = null_mut();
        // SAFETY: duplicates into a primary token, closed by `Handle`.
        if unsafe {
            DuplicateTokenEx(
                own.0,
                0x0200_0000, // MAXIMUM_ALLOWED
                null_mut(),
                SecurityImpersonation,
                TokenPrimary,
                &mut token,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let token = Handle(token);
        // SAFETY: a DWORD session ID; LocalSystem holds SeTcbPrivilege.
        if unsafe {
            SetTokenInformation(
                token.0,
                TokenSessionId,
                (&session as *const u32).cast(),
                size_of::<u32>() as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }

        let mut desktop = wide(r"winsta0\default");
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            lpDesktop: desktop.as_mut_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        let application = wide(&path.to_string_lossy());
        let mut command_line = wide(&format!("\"{}\"", path.display()));
        let mut process = PROCESS_INFORMATION {
            hProcess: null_mut(),
            hThread: null_mut(),
            dwProcessId: 0,
            dwThreadId: 0,
        };
        // SAFETY: every pointer refers to a live buffer; handles are closed
        // below or kept in `Helper`.
        if unsafe {
            CreateProcessAsUserW(
                token.0,
                application.as_ptr(),
                command_line.as_mut_ptr(),
                null_mut(),
                null_mut(),
                0,
                CREATE_NO_WINDOW | CREATE_SUSPENDED,
                null_mut(),
                null_mut(),
                &startup,
                &mut process,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let process_handle = Handle(process.hProcess);
        let thread = Handle(process.hThread);
        if let Some(job) = &self.job {
            // SAFETY: both handles are open.
            unsafe { AssignProcessToJobObject(job.0, process_handle.0) };
        }
        // SAFETY: the suspended main thread of the new process.
        unsafe { ResumeThread(thread.0) };
        Ok(Helper {
            process: process_handle,
            session,
            started: Instant::now(),
        })
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Some(helper) = self.helper.take() {
            // SAFETY: the helper's process handle, still open.
            unsafe { TerminateProcess(helper.process.0, 0) };
        }
    }
}

/// A job that ends its processes when its last handle closes.
fn kill_on_close_job() -> Option<Handle> {
    // SAFETY: an unnamed job; the handle is closed by `Handle`.
    let job = Handle(unsafe { CreateJobObjectW(null_mut(), std::ptr::null()) });
    if job.0.is_null() {
        return None;
    }
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: the structure and its size match the information class.
    let set = unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } != 0;
    set.then_some(job)
}
