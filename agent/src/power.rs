// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sleep, wake and network changes, reported to the core, so sessions end in
//! order before this PC sleeps and come back as soon as it wakes or the
//! network changes (see the core's `runtime/reconnect.rs`).
//!
//! - Sleep and wake: `PowerRegisterSuspendResumeNotification` with a
//!   callback, so no window is needed. `PBT_APMSUSPEND` goes to
//!   `Core::system_will_sleep` at once (Windows waits for the callback, and
//!   the network goes soon after); `PBT_APMRESUMEAUTOMATIC` /
//!   `PBT_APMRESUMESUSPEND` to `Core::network_changed`.
//! - Network: `NotifyUnicastIpAddressChange` for IPv4: an address came or
//!   went (Wi-Fi joined, DHCP, a VPN, a cable). Such changes come in bursts,
//!   so the core hears about them once things have been quiet for a moment.

use rinflow_core::Core;
use std::ffi::c_void;
use std::sync::Weak;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    CancelMibChangeNotify2, MIB_NOTIFICATION_TYPE, MIB_UNICASTIPADDRESS_ROW,
    NotifyUnicastIpAddressChange,
};
use windows_sys::Win32::Networking::WinSock::AF_INET;
use windows_sys::Win32::System::Power::{
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY, PowerRegisterSuspendResumeNotification,
    PowerUnregisterSuspendResumeNotification,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
};

/// How long the network has to stay unchanged before the core hears of it.
const SETTLE: Duration = Duration::from_millis(800);

/// What the callbacks share: the core, and the thread that lets bursts of
/// network changes settle.
struct Context {
    core: Weak<Core>,
    changes: mpsc::Sender<()>,
}

/// Watches until dropped.
pub struct PowerWatch {
    power: HPOWERNOTIFY,
    addresses: HANDLE,
    /// Handed to both callbacks; freed once neither can run any more.
    context: *mut Context,
    settle: Option<JoinHandle<()>>,
}

// The raw handles and the context are only touched on drop, after the
// callbacks are unregistered.
unsafe impl Send for PowerWatch {}

impl PowerWatch {
    pub fn start(core: Weak<Core>) -> Self {
        let (changes, settled) = mpsc::channel::<()>();
        let settle_core = core.clone();
        let settle = std::thread::Builder::new()
            .name("rinflow-network".to_owned())
            .spawn(move || {
                // Ends when the watch is dropped (the sender goes with it).
                while settled.recv().is_ok() {
                    while settled.recv_timeout(SETTLE).is_ok() {}
                    if let Some(core) = settle_core.upgrade() {
                        let _ = core.network_changed();
                    }
                }
            })
            .ok();
        let context = Box::into_raw(Box::new(Context { core, changes }));

        let mut power: HPOWERNOTIFY = 0;
        let mut parameters = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(power_changed),
            Context: context.cast(),
        };
        // SAFETY: with DEVICE_NOTIFY_CALLBACK the recipient is the parameter
        // block, read during the call; `context` outlives the registration.
        let result = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                (&raw mut parameters).cast(),
                (&raw mut power).cast(),
            )
        };
        if result != ERROR_SUCCESS {
            log!("no sleep and wake notifications ({result})");
            power = 0;
        }

        let mut addresses: HANDLE = std::ptr::null_mut();
        // SAFETY: `context` outlives the registration (see `Drop`).
        let result = unsafe {
            NotifyUnicastIpAddressChange(
                AF_INET,
                Some(address_changed),
                context.cast_const().cast(),
                false,
                &mut addresses,
            )
        };
        if result != ERROR_SUCCESS {
            log!("no network change notifications ({result})");
            addresses = std::ptr::null_mut();
        }

        Self {
            power,
            addresses,
            context,
            settle,
        }
    }
}

impl Drop for PowerWatch {
    fn drop(&mut self) {
        // SAFETY: handles from the registrations above. Both calls return
        // only once no callback runs any more, so the context can go.
        unsafe {
            if self.power != 0 {
                PowerUnregisterSuspendResumeNotification(self.power);
            }
            if !self.addresses.is_null() {
                CancelMibChangeNotify2(self.addresses);
            }
            drop(Box::from_raw(self.context));
        }
        if let Some(thread) = self.settle.take() {
            let _ = thread.join();
        }
    }
}

unsafe extern "system" fn power_changed(
    context: *const c_void,
    kind: u32,
    _setting: *const c_void,
) -> u32 {
    // SAFETY: the context registered in `start`, alive while registered.
    let context = unsafe { &*context.cast::<Context>() };
    match kind {
        PBT_APMSUSPEND => {
            // Now, before the network goes: peers get control back and
            // release what they held.
            if let Some(core) = context.core.upgrade() {
                let _ = core.system_will_sleep();
            }
        }
        PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
            let _ = context.changes.send(());
        }
        _ => {}
    }
    ERROR_SUCCESS
}

unsafe extern "system" fn address_changed(
    context: *const c_void,
    _row: *const MIB_UNICASTIPADDRESS_ROW,
    _kind: MIB_NOTIFICATION_TYPE,
) {
    // SAFETY: the context registered in `start`, alive while registered.
    let context = unsafe { &*context.cast::<Context>() };
    let _ = context.changes.send(());
}
