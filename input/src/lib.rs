// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Input beyond a normal process, for Rinflow on Windows.
//!
//! `rinflow-agent` runs as the signed-in user, and Windows keeps such a
//! process from playing input into:
//! - windows of elevated programs (Task Manager, installers): UIPI,
//! - UAC prompts and the lock screen, which live on the Winlogon desktop.
//!
//! So, as Mouse Without Borders and Deskflow do, a process running as
//! LocalSystem does it instead. Unlike them, only the injection runs with
//! that privilege; the network, pairing, files and UI stay in the agent:
//!
//! ```text
//! rinflow-agent (user)  ── named pipe, this crate's protocol ──►  rinflow-input (SYSTEM,
//!                                                                    in the user's session)
//!                                                rinflow-service (SYSTEM service) starts it
//! ```
//!
//! - `rinflow-service` keeps one `rinflow-input` running in the active
//!   console session and restarts it when the session changes or it exits.
//! - `rinflow-input` takes already computed input (`SendInput` values,
//!   or a motion in pixels) from the agent of its own session, and plays it
//!   on whichever desktop has input. It plays nothing unless the machine-wide
//!   setting allows it ([`ELEVATED_INPUT_VALUE`], which only administrators
//!   can change), and on the Winlogon desktop only while a peer is driving
//!   this PC ([`ClientMessage::RemoteControl`]).
//! - The agent uses it when it can connect, and plays input itself otherwise
//!   (a portable copy, or the setting is off).
//!
//! Both ends check who is at the other end of the pipe: its executable must
//! be the other Rinflow program in the same folder (Program Files when
//! installed, where only administrators can write), in the same session.

pub mod log;
pub mod win;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// Stored in `dwExtraInfo` of every injected event ("LCIN"), so the agent's
/// own low-level hooks can tell played input from the user's hardware.
pub const INJECTED_MARKER: usize = 0x4C43_494E;

/// The service's name (`sc.exe`, the installer).
pub const SERVICE_NAME: &str = "RinflowInput";
pub const SERVICE_DISPLAY_NAME: &str = "Rinflow Input";

/// `HKEY_LOCAL_MACHINE` key and DWORD value: 1 lets `rinflow-input` play
/// input (elevated windows, UAC prompts, the lock screen). Written by the
/// installer and by `rinflow-service --elevated-input on|off`, both
/// running as an administrator.
pub const REGISTRY_KEY: &str = r"SOFTWARE\Rinflow";
pub const ELEVATED_INPUT_VALUE: &str = "ElevatedInput";

pub const AGENT_EXE: &str = "rinflow-agent.exe";
pub const INPUT_EXE: &str = "rinflow-input.exe";
pub const SERVICE_EXE: &str = "rinflow-service.exe";

/// The pipe of the input helper in a session.
pub fn pipe_name(session: u32) -> String {
    format!(r"\\.\pipe\Rinflow.Input.{session}")
}

/// Pipe protocol version, exchanged in `Hello`.
pub const PROTOCOL_VERSION: u16 = 1;

/// Frames are a little-endian `u32` length, then the message; never longer.
pub const MAX_FRAME: usize = 64;

/// Input as `SendInput` takes it, without the fields the helper fills in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawInput {
    Mouse {
        dx: i32,
        dy: i32,
        data: u32,
        flags: u32,
    },
    Key {
        vk: u16,
        scan: u16,
        flags: u32,
    },
}

const MOUSE_FLAGS: u32 = MOUSEEVENTF_MOVE
    | MOUSEEVENTF_ABSOLUTE
    | MOUSEEVENTF_VIRTUALDESK
    | MOUSEEVENTF_LEFTDOWN
    | MOUSEEVENTF_LEFTUP
    | MOUSEEVENTF_RIGHTDOWN
    | MOUSEEVENTF_RIGHTUP
    | MOUSEEVENTF_MIDDLEDOWN
    | MOUSEEVENTF_MIDDLEUP
    | MOUSEEVENTF_XDOWN
    | MOUSEEVENTF_XUP
    | MOUSEEVENTF_WHEEL
    | MOUSEEVENTF_HWHEEL;
const KEY_FLAGS: u32 =
    KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP | KEYEVENTF_SCANCODE | KEYEVENTF_UNICODE;

impl RawInput {
    /// Takes the fields of an `INPUT` the agent built.
    pub fn from_input(input: &INPUT) -> Option<Self> {
        // SAFETY: the union member read matches the `type` field.
        unsafe {
            match input.r#type {
                INPUT_MOUSE => {
                    let mi = input.Anonymous.mi;
                    Some(Self::Mouse {
                        dx: mi.dx,
                        dy: mi.dy,
                        data: mi.mouseData,
                        flags: mi.dwFlags,
                    })
                }
                INPUT_KEYBOARD => {
                    let ki = input.Anonymous.ki;
                    Some(Self::Key {
                        vk: ki.wVk,
                        scan: ki.wScan,
                        flags: ki.dwFlags,
                    })
                }
                _ => None,
            }
        }
    }

    /// The `INPUT` to play, marked as Rinflow's; `None` for flags this
    /// program never sends.
    pub fn to_input(self) -> Option<INPUT> {
        match self {
            Self::Mouse {
                dx,
                dy,
                data,
                flags,
            } if flags & !MOUSE_FLAGS == 0 => Some(INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx,
                        dy,
                        mouseData: data,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: INJECTED_MARKER,
                    },
                },
            }),
            Self::Key { vk, scan, flags } if flags & !KEY_FLAGS == 0 => Some(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        wScan: scan,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: INJECTED_MARKER,
                    },
                },
            }),
            _ => None,
        }
    }
}

/// Agent to helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientMessage {
    Hello {
        version: u16,
    },
    Input(RawInput),
    /// Move the pointer by this many physical pixels from where it is on the
    /// input desktop (the agent cannot see the pointer on the Winlogon
    /// desktop).
    MoveBy {
        dx: i32,
        dy: i32,
    },
    /// Whether a peer is driving this PC now. Input reaches the Winlogon
    /// desktop (UAC prompts, the lock screen) only while one is.
    RemoteControl {
        active: bool,
    },
}

/// Helper to agent, after `Hello` and on every change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerMessage {
    State {
        /// The machine-wide setting allows playing input.
        enabled: bool,
        /// The input desktop is not the user's (UAC prompt, lock screen).
        secure: bool,
    },
}

impl ClientMessage {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(24);
        match *self {
            Self::Hello { version } => {
                out.push(1);
                out.extend_from_slice(&version.to_le_bytes());
            }
            Self::Input(RawInput::Mouse {
                dx,
                dy,
                data,
                flags,
            }) => {
                out.push(2);
                out.push(0);
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
                out.extend_from_slice(&data.to_le_bytes());
                out.extend_from_slice(&flags.to_le_bytes());
            }
            Self::Input(RawInput::Key { vk, scan, flags }) => {
                out.push(2);
                out.push(1);
                out.extend_from_slice(&vk.to_le_bytes());
                out.extend_from_slice(&scan.to_le_bytes());
                out.extend_from_slice(&flags.to_le_bytes());
            }
            Self::MoveBy { dx, dy } => {
                out.push(3);
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            Self::RemoteControl { active } => {
                out.push(4);
                out.push(u8::from(active));
            }
        }
        frame(out)
    }

    /// A message without its length prefix; `None` for anything malformed.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader(payload);
        let message = match reader.u8()? {
            1 => Self::Hello {
                version: reader.u16()?,
            },
            2 => Self::Input(match reader.u8()? {
                0 => RawInput::Mouse {
                    dx: reader.i32()?,
                    dy: reader.i32()?,
                    data: reader.u32()?,
                    flags: reader.u32()?,
                },
                1 => RawInput::Key {
                    vk: reader.u16()?,
                    scan: reader.u16()?,
                    flags: reader.u32()?,
                },
                _ => return None,
            }),
            3 => Self::MoveBy {
                dx: reader.i32()?,
                dy: reader.i32()?,
            },
            4 => Self::RemoteControl {
                active: reader.flag()?,
            },
            _ => return None,
        };
        reader.0.is_empty().then_some(message)
    }
}

impl ServerMessage {
    pub fn encode(&self) -> Vec<u8> {
        let Self::State { enabled, secure } = *self;
        frame(vec![1, u8::from(enabled), u8::from(secure)])
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader(payload);
        let message = match reader.u8()? {
            1 => Self::State {
                enabled: reader.flag()?,
                secure: reader.flag()?,
            },
            _ => return None,
        };
        reader.0.is_empty().then_some(message)
    }
}

fn frame(payload: Vec<u8>) -> Vec<u8> {
    let mut out = (payload.len() as u32).to_le_bytes().to_vec();
    out.extend(payload);
    out
}

/// Reads one frame's payload; `None` at the end of the stream, an error for
/// an oversized frame.
pub async fn read_frame<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Vec<u8>>> {
    use tokio::io::AsyncReadExt;
    let mut length = [0u8; 4];
    match reader.read_exact(&mut length).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|[byte]| byte)
    }
    fn flag(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
    fn u16(&mut self) -> Option<u16> {
        self.take().map(u16::from_le_bytes)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
    fn i32(&mut self) -> Option<i32> {
        self.take().map(i32::from_le_bytes)
    }
}

/// The virtual desktop, in physical pixels (the caller must be per-monitor
/// DPI aware).
#[derive(Clone, Copy, Debug)]
pub struct VirtualDesktop {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl VirtualDesktop {
    pub fn current() -> Self {
        // SAFETY: plain system metric queries.
        unsafe {
            Self {
                x: GetSystemMetrics(SM_XVIRTUALSCREEN),
                y: GetSystemMetrics(SM_YVIRTUALSCREEN),
                width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
                height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
            }
        }
    }

    /// An absolute move to a physical pixel, clamped onto the desktop.
    /// Played as absolute, so Windows applies no pointer acceleration.
    pub fn move_to(&self, x: i32, y: i32) -> RawInput {
        let x = x.clamp(self.x, self.x + self.width - 1);
        let y = y.clamp(self.y, self.y + self.height - 1);
        RawInput::Mouse {
            dx: normalize(x, self.x, self.width),
            dy: normalize(y, self.y, self.height),
            data: 0,
            flags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        }
    }
}

/// Maps a virtual-desktop pixel to SendInput's 0…65535 range, aiming at the
/// pixel's centre so Windows' conversion back lands on the same pixel.
pub fn normalize(value: i32, origin: i32, extent: i32) -> i32 {
    let extent = extent.max(1) as f64;
    (((value - origin) as f64 + 0.5) * 65536.0 / extent).clamp(0.0, 65535.0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_and_reject_garbage() {
        for message in [
            ClientMessage::Hello { version: 1 },
            ClientMessage::Input(RawInput::Mouse {
                dx: -5,
                dy: 70_000,
                data: 120,
                flags: MOUSEEVENTF_WHEEL,
            }),
            ClientMessage::Input(RawInput::Key {
                vk: 0,
                scan: 0x1E,
                flags: KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP,
            }),
            ClientMessage::MoveBy { dx: 3, dy: -2 },
            ClientMessage::RemoteControl { active: true },
        ] {
            let bytes = message.encode();
            assert_eq!(ClientMessage::decode(&bytes[4..]), Some(message));
            // Truncated or padded frames are refused.
            assert_eq!(ClientMessage::decode(&bytes[4..bytes.len() - 1]), None);
            let mut padded = bytes[4..].to_vec();
            padded.push(0);
            assert_eq!(ClientMessage::decode(&padded), None);
        }
        let state = ServerMessage::State {
            enabled: true,
            secure: false,
        };
        assert_eq!(ServerMessage::decode(&state.encode()[4..]), Some(state));
        assert_eq!(ClientMessage::decode(&[4, 2]), None);
        assert_eq!(ClientMessage::decode(&[9]), None);
    }

    #[test]
    fn only_known_flags_are_played() {
        let wheel = RawInput::Mouse {
            dx: 0,
            dy: 0,
            data: 120,
            flags: MOUSEEVENTF_WHEEL,
        };
        let input = wheel.to_input().unwrap();
        assert_eq!(RawInput::from_input(&input), Some(wheel));
        assert!(
            RawInput::Mouse {
                dx: 0,
                dy: 0,
                data: 0,
                flags: 0x8000_0000
            }
            .to_input()
            .is_none()
        );
        assert!(
            RawInput::Key {
                vk: 0,
                scan: 0,
                flags: 0x100
            }
            .to_input()
            .is_none()
        );
    }

    #[test]
    fn normalized_coordinates_map_back_to_the_same_pixel() {
        // Windows converts back with floor(n * width / 65536).
        for (origin, width) in [(0, 2880), (-1920, 4800), (0, 1366)] {
            for x in [origin, origin + 1, origin + width / 2, origin + width - 1] {
                let n = normalize(x, origin, width) as i64;
                let back = origin as i64 + n * width as i64 / 65536;
                assert_eq!(back, x as i64, "origin {origin} width {width}");
            }
        }
    }
}
