//! Privileged deny-only network enforcement for caller-owned AppContainer Jobs.
use crate::execution::{ExecutionError, ExecutionErrorCategory};
use std::{
    ffi::OsStr,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
};
mod client;
mod filtering;
mod identity;
mod installation;
mod pipe;
mod protocol;
mod service;
pub(super) use client::Guard;
pub(crate) const SERVICE: &str = "TapidNetworkGuardV1";
pub(super) const PIPE: &str = r"\\.\pipe\Tapid.NetworkGuard.v1";
const PRIVATE_ARG: &str = "--private-windows-network-guard-service-v1";
pub(super) fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}
pub(super) fn raw(handle: &OwnedHandle) -> isize {
    handle.as_raw_handle() as isize
}
pub(super) fn owned(handle: isize) -> Result<OwnedHandle, u32> {
    if handle == 0 || handle == -1 {
        Err(unsafe { windows_sys::Win32::Foundation::GetLastError() })
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle as *mut std::ffi::c_void) })
    }
}
pub(super) fn failure(stage: &str, error: u32) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCategory::UnsupportedContainment,
        format!(
            "Windows network guard {stage} failed ({error}); install/start the administrator-managed guard with 'tapid sandbox install'; no uncontained fallback is available"
        ),
    )
}
/// One-time administrator setup or read-only inspection of Windows network enforcement.
#[derive(Clone, Copy, Debug)]
pub enum WindowsNetworkGuardAction {
    Install,
    Status,
    Uninstall,
}
/// Observed Windows service state, separate from per-execution enforcement evidence.
#[derive(Debug)]
pub struct WindowsNetworkGuardStatus {
    pub installed: bool,
    pub running: bool,
    pub active_executions: u32,
    pub retained_filters: u32,
    pub executable: PathBuf,
}
/// Manage the protected Windows deny service. Install/uninstall require administrator rights.
pub fn windows_network_guard(
    action: WindowsNetworkGuardAction,
) -> Result<WindowsNetworkGuardStatus, String> {
    installation::manage(action)
}
pub(crate) fn dispatch_private_service() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|a| a == PRIVATE_ARG) {
        if args.len() != 2 {
            std::process::exit(2);
        }
        std::process::exit(match service::dispatch() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("Windows network guard service failed ({error})");
                1
            }
        });
    }
}
