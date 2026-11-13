// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The sound that goes with a peer's picture: what the computer behind a
//! KVM plays comes out of this PC's speakers ([`Player`]), and, if the user
//! turned it on, this PC's microphone is the computer's ([`Microphone`]).
//!
//! The core carries the sound as it is (`rinflow_core::MediaAudio`): signed
//! 16-bit samples at 48 kHz, a few milliseconds a packet, not sent again
//! when lost. Both ends here are WASAPI streams in shared mode on the
//! default devices, in that format: Windows converts to and from whatever
//! the device runs at. Each has a thread of its own, which starts over when
//! the device goes (headphones unplugged) and ends when its handle is
//! dropped.
//!
//! **Late is useless** here as for the picture. The player keeps a little
//! sound ahead ([`AHEAD`]) to cover the network's unevenness, plays silence
//! when it runs out and starts again once that much is there; when more
//! than [`MOST`] waits (the sender's clock runs a little faster than this
//! PC's, or packets came in a burst), it skips to [`AHEAD`].

use rinflow_core::MediaAudio;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, EDataFlow,
    IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    WAVE_FORMAT_PCM, WAVEFORMATEX, eCapture, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

const RATE: u32 = 48_000;
/// What the computer plays: stereo.
const SPEAKER_CHANNELS: u16 = 2;
/// This PC's microphone: mono, 10 ms a packet.
const MICROPHONE_CHANNELS: u16 = 1;
const MICROPHONE_PACKET: usize = 960;
/// Stereo frames kept ahead of the speakers: 40 ms.
const AHEAD: usize = RATE as usize * 40 / 1000;
/// And the most that may wait: 150 ms.
const MOST: usize = RATE as usize * 150 / 1000;
/// The device's own buffer, in 100 ns: 20 ms.
const DEVICE_BUFFER: i64 = 200_000;
const RETRY: Duration = Duration::from_secs(1);

/// The sound waiting for the speakers, as stereo frames.
#[derive(Default)]
struct Waiting {
    frames: VecDeque<[i16; 2]>,
    /// Enough was there to start; false again once it ran out.
    playing: bool,
}

impl Waiting {
    fn push(&mut self, sound: &MediaAudio) {
        let samples = sound
            .samples
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| i16::from_le_bytes(*sample));
        if sound.channels == 1 {
            self.frames.extend(samples.map(|sample| [sample; 2]));
        } else {
            let mut samples = samples;
            while let (Some(left), Some(right)) = (samples.next(), samples.next()) {
                self.frames.push_back([left, right]);
            }
        }
        if self.frames.len() > MOST {
            let late = self.frames.len() - AHEAD;
            self.frames.drain(..late);
        }
    }

    /// Fills `out` (interleaved stereo) with what waits, as far as it
    /// goes; how many frames that was.
    fn take(&mut self, out: &mut [i16]) -> usize {
        if !self.playing {
            if self.frames.len() < AHEAD {
                return 0;
            }
            self.playing = true;
        }
        let mut taken = 0;
        for frame in out.as_chunks_mut::<2>().0 {
            let Some(next) = self.frames.pop_front() else {
                self.playing = false;
                break;
            };
            *frame = next;
            taken += 1;
        }
        taken
    }
}

/// Plays a peer's sound on this PC's default speakers until dropped.
pub struct Player {
    waiting: Arc<Mutex<Waiting>>,
    stop: Arc<AtomicBool>,
}

impl Player {
    pub fn start() -> Self {
        let waiting = Arc::new(Mutex::new(Waiting::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_waiting, thread_stop) = (waiting.clone(), stop.clone());
        spawn("viewer-speaker", stop.clone(), move || {
            play(&thread_waiting, &thread_stop)
        });
        Self { waiting, stop }
    }

    pub fn push(&self, sound: &MediaAudio) {
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.push(sound);
        }
    }

    /// Frames waiting for the speakers.
    #[cfg(debug_assertions)]
    pub fn waiting(&self) -> usize {
        self.waiting
            .lock()
            .map_or(0, |waiting| waiting.frames.len())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Hears this PC's default microphone until dropped: `on_sound` gets a
/// packet of mono samples at a time, on the microphone's thread.
pub struct Microphone {
    stop: Arc<AtomicBool>,
}

impl Microphone {
    pub fn start(on_sound: impl FnMut(&[u8]) + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let mut on_sound = on_sound;
        spawn("viewer-microphone", stop.clone(), move || {
            record(&mut on_sound, &thread_stop)
        });
        Self { stop }
    }
}

impl Drop for Microphone {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Runs `stream` on a thread of its own, again after a while whenever it
/// ends (no device, or the device went), until `stop`.
fn spawn(
    name: &str,
    stop: Arc<AtomicBool>,
    mut stream: impl FnMut() -> windows::core::Result<()> + Send + 'static,
) {
    let label = name.to_owned();
    let result = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // SAFETY: COM for WASAPI on this thread, undone when it ends.
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            let mut said = false;
            while !stop.load(Ordering::Acquire) {
                if let Err(error) = stream()
                    && !said
                {
                    // Once: no device may stay that way.
                    log!("{label}: {error}");
                    said = true;
                }
                let mut waited = Duration::ZERO;
                while waited < RETRY && !stop.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(50));
                    waited += Duration::from_millis(50);
                }
            }
            if com {
                // SAFETY: matches the initialization above.
                unsafe { CoUninitialize() };
            }
        });
    if let Err(error) = result {
        log!("{name}: cannot start its thread: {error}");
    }
}

/// An event handle closed when dropped.
struct Event(HANDLE);

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle is this struct's own.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A stream on the default device of `flow`, in 16-bit samples at 48 kHz
/// with `channels`, which signals `Event` whenever it wants or has sound.
///
/// # Safety
/// COM must be initialized on this thread.
unsafe fn open(flow: EDataFlow, channels: u16) -> windows::core::Result<(IAudioClient, Event)> {
    // SAFETY: COM calls on interfaces just obtained; `format` outlives the
    // call that reads it.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(flow, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let block = channels * 2;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: channels,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            DEVICE_BUFFER,
            0,
            &format,
            None,
        )?;
        let event = Event(CreateEventW(None, false, false, None)?);
        client.SetEventHandle(event.0)?;
        Ok((client, event))
    }
}

fn play(waiting: &Mutex<Waiting>, stop: &AtomicBool) -> windows::core::Result<()> {
    // SAFETY: COM is initialized on this thread (`spawn`); the buffer
    // WASAPI hands out holds `free` stereo frames of 16-bit samples until
    // it is released.
    unsafe {
        let (client, event) = open(eRender, SPEAKER_CHANNELS)?;
        let size = client.GetBufferSize()?;
        let render: IAudioRenderClient = client.GetService()?;
        client.Start()?;
        while !stop.load(Ordering::Acquire) {
            if WaitForSingleObject(event.0, 200) != WAIT_OBJECT_0 {
                continue;
            }
            let free = size.saturating_sub(client.GetCurrentPadding()?);
            if free == 0 {
                continue;
            }
            let buffer = render.GetBuffer(free)?;
            let out = std::slice::from_raw_parts_mut(buffer.cast::<i16>(), free as usize * 2);
            let taken = waiting.lock().map_or(0, |mut waiting| waiting.take(out));
            // Nothing to play: nothing is written, and the device plays
            // silence by itself.
            render.ReleaseBuffer(taken as u32, 0)?;
        }
        client.Stop()?;
    }
    Ok(())
}

fn record(on_sound: &mut impl FnMut(&[u8]), stop: &AtomicBool) -> windows::core::Result<()> {
    // SAFETY: COM is initialized on this thread (`spawn`); the buffer
    // WASAPI hands out holds `frames` mono 16-bit samples until it is
    // released.
    unsafe {
        let (client, event) = open(eCapture, MICROPHONE_CHANNELS)?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let mut packet: Vec<u8> = Vec::with_capacity(MICROPHONE_PACKET * 2);
        while !stop.load(Ordering::Acquire) {
            if WaitForSingleObject(event.0, 200) != WAIT_OBJECT_0 {
                continue;
            }
            while capture.GetNextPacketSize()? > 0 {
                let mut data = std::ptr::null_mut();
                let (mut frames, mut flags) = (0u32, 0u32);
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let bytes = frames as usize * 2;
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    // Muted: nothing to send, and what was begun is stale.
                    packet.clear();
                } else {
                    packet.extend_from_slice(std::slice::from_raw_parts(data, bytes));
                }
                capture.ReleaseBuffer(frames)?;
                while packet.len() >= MICROPHONE_PACKET {
                    on_sound(&packet[..MICROPHONE_PACKET]);
                    packet.drain(..MICROPHONE_PACKET);
                }
            }
        }
        client.Stop()?;
    }
    Ok(())
}

/// The microphone's channels, for the core.
pub const fn microphone_channels() -> u8 {
    MICROPHONE_CHANNELS as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sound(channels: u8, frames: usize, value: i16) -> MediaAudio {
        let samples: Vec<u8> = std::iter::repeat_n(value.to_le_bytes(), frames * channels as usize)
            .flatten()
            .collect();
        MediaAudio {
            channels,
            sequence: 0,
            samples: samples.into(),
        }
    }

    #[test]
    fn sound_starts_once_some_is_ahead_and_again_after_it_ran_out() {
        let mut waiting = Waiting::default();
        let mut out = [0i16; 2 * 480];
        // 10 ms is not enough to start with.
        waiting.push(&sound(2, 480, 7));
        assert_eq!(waiting.take(&mut out), 0);
        for _ in 0..3 {
            waiting.push(&sound(2, 480, 7));
        }
        assert_eq!(waiting.take(&mut out), 480);
        assert_eq!(out[0], 7);
        // It plays on down to the last frame, then waits to be ahead again.
        for _ in 0..3 {
            assert_eq!(waiting.take(&mut out), 480);
        }
        assert_eq!(waiting.take(&mut out), 0);
        waiting.push(&sound(2, 480, 7));
        assert_eq!(waiting.take(&mut out), 0);
    }

    #[test]
    fn mono_plays_on_both_sides_and_what_is_late_is_skipped() {
        let mut waiting = Waiting::default();
        waiting.push(&sound(1, AHEAD, 3));
        let mut out = [0i16; 4];
        assert_eq!(waiting.take(&mut out), 2);
        assert_eq!(out, [3, 3, 3, 3]);
        // Far more than may wait: only the newest is kept.
        let mut waiting = Waiting::default();
        for value in 0..40 {
            waiting.push(&sound(2, 480, value));
        }
        assert!(waiting.frames.len() <= MOST);
        assert_eq!(waiting.frames.back(), Some(&[39, 39]));
        assert!(waiting.frames.front().unwrap()[0] > 30);
    }
}
