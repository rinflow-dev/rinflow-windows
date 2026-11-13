// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Frames of a picture decoded on the graphics processor, with Media
//! Foundation's decoder for the codec (for H.265 the one in the HEVC Video
//! Extensions, for H.264 the one in Windows).
//!
//! The decoder is given the Direct3D 11 device the window draws with, so it
//! decodes into textures there (DXVA) and the processor never touches a
//! picture. Low-latency mode makes it put out each frame as soon as it has
//! it, instead of holding a few back for reordering the KVM's encoder never
//! does.

use super::codec::Codec;
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaType, IMFSample, IMFTransform,
    MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
    MF_LOW_LATENCY, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE, MF_VERSION,
    MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFSTARTUP_LITE, MFShutdown, MFStartup, MFT_CATEGORY_VIDEO_DECODER,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_COMMAND_FLUSH,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoArea, MFVideoFormat_H264, MFVideoFormat_HEVC,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::{Error, Interface, Result};

/// One decoded picture: a slice of a texture array of the decoder's. The
/// sample is kept until the next picture, so the decoder does not reuse
/// the slice while it is being drawn.
pub struct Picture {
    _sample: IMFSample,
    pub texture: ID3D11Texture2D,
    pub slice: u32,
    /// The part of the texture that is the picture (decoders round the
    /// height up: 1080 lines are coded as 1088).
    pub visible: (u32, u32, u32, u32),
}

/// Media Foundation, started for as long as one is alive.
struct Platform;

impl Platform {
    fn start() -> Result<Self> {
        // SAFETY: reference-counted start, matched by the drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE)? };
        Ok(Self)
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        // SAFETY: matches the start.
        unsafe {
            let _ = MFShutdown();
        }
    }
}

pub struct Decoder {
    transform: IMFTransform,
    // Kept for as long as the decoder uses the device through it.
    _manager: IMFDXGIDeviceManager,
    pub codec: Codec,
    /// The decoder's own name for itself, for the log.
    pub name: String,
    visible: (u32, u32, u32, u32),
    /// Waiting for a key frame before anything goes in (at the start, and
    /// after the decoder lost its place).
    needs_key_frame: bool,
    // Last: Media Foundation stops after everything above is released.
    _platform: Platform,
}

/// Hardware decoders first (they come sorted), then Microsoft's, which
/// uses DXVA itself when it gets a device.
fn find_decoder(codec: Codec) -> Result<(IMFTransform, String)> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: match codec {
            Codec::H264 => MFVideoFormat_H264,
            Codec::H265 => MFVideoFormat_HEVC,
        },
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: MFTEnumEx fills an array of `count` activates, which is freed
    // below after each one is taken out of it.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut activates,
            &mut count,
        )?;
        let list: Vec<IMFActivate> = (0..count as usize)
            .filter_map(|index| (*activates.add(index)).take())
            .collect();
        CoTaskMemFree(Some(activates as *const _));
        let mut last_error = Error::new(E_FAIL, "no decoder");
        for activate in list {
            let name = friendly_name(&activate);
            match activate.ActivateObject::<IMFTransform>() {
                Ok(transform) => return Ok((transform, name)),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }
}

fn friendly_name(activate: &IMFActivate) -> String {
    use windows::Win32::Media::MediaFoundation::MFT_FRIENDLY_NAME_Attribute;
    let mut buffer = [0u16; 128];
    let mut length = 0u32;
    // SAFETY: the buffer is as long as said.
    unsafe {
        if activate
            .GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buffer, Some(&mut length))
            .is_ok()
        {
            return String::from_utf16_lossy(&buffer[..length as usize]);
        }
    }
    String::from("?")
}

/// Whether a decoder for `codec` is installed. The HEVC Video Extensions
/// may not be.
pub fn available(codec: Codec) -> bool {
    let Ok(_platform) = Platform::start() else {
        return false;
    };
    find_decoder(codec).is_ok()
}

impl Decoder {
    /// `size` is what the picture probably is (the peer's screen): the
    /// decoder needs a size before the first frame, and says when the
    /// stream turns out to have another.
    pub fn new(codec: Codec, device: &ID3D11Device, size: (u32, u32)) -> Result<Self> {
        let platform = Platform::start()?;
        let (transform, name) = find_decoder(codec)?;
        // SAFETY: COM calls on objects this function owns.
        unsafe {
            let attributes = transform.GetAttributes()?;
            if attributes.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                return Err(Error::new(
                    E_FAIL,
                    "the decoder cannot decode into textures",
                ));
            }
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);

            let mut token = 0u32;
            let mut manager: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.ok_or_else(|| Error::new(E_FAIL, "no device manager"))?;
            manager.ResetDevice(device, token)?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

            let input: IMFMediaType = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(
                &MF_MT_SUBTYPE,
                match codec {
                    Codec::H264 => &MFVideoFormat_H264,
                    Codec::H265 => &MFVideoFormat_HEVC,
                },
            )?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT64(
                &MF_MT_FRAME_SIZE,
                (u64::from(size.0.max(16)) << 32) | u64::from(size.1.max(16)),
            )?;
            transform.SetInputType(0, &input, 0)?;

            let mut decoder = Self {
                transform,
                _manager: manager,
                codec,
                name,
                visible: (0, 0, 0, 0),
                needs_key_frame: true,
                _platform: platform,
            };
            decoder.choose_output()?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(decoder)
        }
    }

    /// NV12 out, and where the picture is in it. Again whenever the stream
    /// changes (a new size).
    fn choose_output(&mut self) -> Result<()> {
        // SAFETY: COM calls on the decoder.
        unsafe {
            let mut index = 0;
            loop {
                let kind = self.transform.GetOutputAvailableType(0, index)?;
                if kind.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    self.transform.SetOutputType(0, &kind, 0)?;
                    self.visible = visible_area(&kind);
                    return Ok(());
                }
                index += 1;
            }
        }
    }

    /// The size of the picture being decoded; zero until the first frame.
    pub fn size(&self) -> (u32, u32) {
        (self.visible.2, self.visible.3)
    }

    /// From the next key frame on: the decoder lost its place (a frame
    /// missing), so what follows until then cannot be decoded.
    pub fn restart(&mut self) {
        self.needs_key_frame = true;
        // SAFETY: drops what the decoder holds.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
    }

    /// Decodes one frame (NAL units with start codes) and returns the
    /// newest picture that came out, if any did.
    pub fn decode(
        &mut self,
        data: &[u8],
        key_frame: bool,
        timestamp_micros: u64,
    ) -> Result<Option<Picture>> {
        if self.needs_key_frame {
            if !key_frame {
                return Ok(None);
            }
            self.needs_key_frame = false;
        }
        // SAFETY: COM calls on the decoder; the buffer is locked while it is
        // written, for exactly its length.
        unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut target = std::ptr::null_mut();
            buffer.Lock(&mut target, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            // Media Foundation counts in 100 ns.
            sample.SetSampleTime((timestamp_micros as i64).saturating_mul(10))?;

            let mut newest = None;
            if let Err(error) = self.transform.ProcessInput(0, &sample, 0) {
                if error.code() != MF_E_NOTACCEPTING {
                    return Err(error);
                }
                // Full: take out what is ready, then it takes the frame.
                newest = self.drain()?;
                self.transform.ProcessInput(0, &sample, 0)?;
            }
            Ok(self.drain()?.or(newest))
        }
    }

    /// Takes every picture the decoder has ready; returns the newest.
    fn drain(&mut self) -> Result<Option<Picture>> {
        let mut newest = None;
        // SAFETY: COM calls on the decoder. The output buffer's sample and
        // events are taken out of their ManuallyDrop each time.
        unsafe {
            let info = self.transform.GetOutputStreamInfo(0)?;
            let provides = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            if !provides {
                return Err(Error::new(E_FAIL, "the decoder wants memory, not textures"));
            }
            loop {
                let mut output = [MFT_OUTPUT_DATA_BUFFER::default()];
                let mut status = 0u32;
                let result = self.transform.ProcessOutput(0, &mut output, &mut status);
                let sample = std::mem::ManuallyDrop::take(&mut output[0].pSample);
                drop(std::mem::ManuallyDrop::take(&mut output[0].pEvents));
                match result {
                    Ok(()) => {
                        if let Some(sample) = sample {
                            newest = Some(self.picture(sample)?);
                        }
                    }
                    Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                        return Ok(newest);
                    }
                    Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        self.choose_output()?;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }

    fn picture(&self, sample: IMFSample) -> Result<Picture> {
        // SAFETY: COM calls on the decoder's sample; GetResource hands over a
        // reference to the texture, which `from_raw` takes.
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            let dxgi: IMFDXGIBuffer = buffer.cast()?;
            let mut texture = std::ptr::null_mut();
            dxgi.GetResource(&ID3D11Texture2D::IID, &mut texture)?;
            let texture = ID3D11Texture2D::from_raw(texture);
            let slice = dxgi.GetSubresourceIndex()?;
            Ok(Picture {
                _sample: sample,
                texture,
                slice,
                visible: self.visible,
            })
        }
    }
}

/// Where the picture is in the decoder's output: its display aperture if
/// it says, else the whole frame.
fn visible_area(kind: &IMFMediaType) -> (u32, u32, u32, u32) {
    // SAFETY: reads attributes; the blob is read into a buffer of its size.
    unsafe {
        let size = kind.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
        let (width, height) = ((size >> 32) as u32, size as u32);
        let mut area = MFVideoArea::default();
        let bytes = std::slice::from_raw_parts_mut(
            (&mut area as *mut MFVideoArea).cast::<u8>(),
            size_of::<MFVideoArea>(),
        );
        if kind
            .GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None)
            .is_ok()
            && area.Area.cx > 0
            && area.Area.cy > 0
        {
            let x = area.OffsetX.value.max(0) as u32;
            let y = area.OffsetY.value.max(0) as u32;
            return (x, y, area.Area.cx as u32, area.Area.cy as u32);
        }
        (0, 0, width, height)
    }
}
