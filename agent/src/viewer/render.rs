// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Draws decoded pictures into the viewer window: the graphics processor's
//! video processor scales the decoder's NV12 texture into the window's
//! swap chain, converting to RGB on the way, with black bars around it.
//! Nothing is queued: each picture is presented as it comes.

use super::decoder::Picture;
use super::geometry;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_VIDEO_COLOR, D3D11_VIDEO_COLOR_0,
    D3D11_VIDEO_COLOR_RGBA, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice,
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11RenderTargetView, ID3D11Texture2D,
    ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_RATIONAL,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIAdapter, IDXGIDevice,
    IDXGIFactory2, IDXGISwapChain1,
};
use windows::core::{Interface, Result};

/// BT.709 with studio range (16 to 235), as HDMI pictures are.
const STREAM_COLOR_SPACE: u32 = (1 << 2) | (1 << 4);
/// Full-range RGB.
const OUTPUT_COLOR_SPACE: u32 = 0;

struct Processor {
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    /// The texture size and output size it was made for.
    input: (u32, u32),
    output: (u32, u32),
}

pub struct Renderer {
    pub device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    swap_chain: IDXGISwapChain1,
    size: (u32, u32),
    processor: Option<Processor>,
    output_view: Option<ID3D11VideoProcessorOutputView>,
    /// The input view of the texture slice drawn last: the decoder cycles
    /// through a few, so one view per slice is made as they come.
    input_views: Vec<(usize, u32, ID3D11VideoProcessorInputView)>,
}

impl Renderer {
    pub fn new(window: HWND, size: (u32, u32)) -> Result<Self> {
        // SAFETY: Direct3D calls on objects this function makes.
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device: ID3D11Device = device.expect("device");
            let context: ID3D11DeviceContext = context.expect("context");
            // The decoder uses the device from its own threads.
            let multithread: ID3D11Multithread = device.cast()?;
            let _ = multithread.SetMultithreadProtected(true);
            let video_device: ID3D11VideoDevice = device.cast()?;
            let video_context: ID3D11VideoContext = context.cast()?;

            let dxgi: IDXGIDevice = device.cast()?;
            let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
            let description = DXGI_SWAP_CHAIN_DESC1 {
                Width: size.0.max(1),
                Height: size.1.max(1),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_NONE,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: 0,
                ..Default::default()
            };
            let swap_chain =
                factory.CreateSwapChainForHwnd(&device, window, &description, None, None)?;
            Ok(Self {
                device,
                context,
                video_device,
                video_context,
                swap_chain,
                size: (size.0.max(1), size.1.max(1)),
                processor: None,
                output_view: None,
                input_views: Vec::new(),
            })
        }
    }

    /// The window's client area changed size.
    pub fn resize(&mut self, size: (u32, u32)) -> Result<()> {
        let size = (size.0.max(1), size.1.max(1));
        if size == self.size {
            return Ok(());
        }
        // Views of the old buffers must go before the buffers can.
        self.output_view = None;
        self.processor = None;
        // SAFETY: resizes the swap chain this renderer owns.
        unsafe {
            self.context.ClearState();
            self.context.Flush();
            self.swap_chain.ResizeBuffers(
                0,
                size.0,
                size.1,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
        }
        self.size = size;
        Ok(())
    }

    /// A black window, before the first picture.
    pub fn clear(&mut self) -> Result<()> {
        // SAFETY: draws into this renderer's back buffer.
        unsafe {
            let buffer: ID3D11Texture2D = self.swap_chain.GetBuffer(0)?;
            let mut view: Option<ID3D11RenderTargetView> = None;
            self.device
                .CreateRenderTargetView(&buffer, None, Some(&mut view))?;
            if let Some(view) = view {
                self.context
                    .ClearRenderTargetView(&view, &[0.0, 0.0, 0.0, 1.0]);
            }
            self.swap_chain.Present(0, DXGI_PRESENT(0)).ok()
        }
    }

    /// Draws a picture to fit the window, and shows it.
    pub fn draw(&mut self, picture: &Picture) -> Result<()> {
        let (x, y, width, height) = picture.visible;
        // SAFETY: Direct3D calls on this renderer's objects and the decoder's
        // texture, which the picture keeps alive.
        unsafe {
            let mut description = Default::default();
            picture.texture.GetDesc(&mut description);
            let input = (description.Width, description.Height);
            self.ensure_processor(input)?;
            let Some(Processor {
                enumerator,
                processor,
                ..
            }) = &self.processor
            else {
                return Ok(());
            };
            let (enumerator, processor) = (enumerator.clone(), processor.clone());
            let input_view = self.input_view(picture, &enumerator)?;
            let output_view = self.output_view()?;
            let processor = &processor;

            let rect = geometry::fit((width, height), self.size);
            let source = RECT {
                left: x as i32,
                top: y as i32,
                right: (x + width) as i32,
                bottom: (y + height) as i32,
            };
            let target = RECT {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            };
            let video = &self.video_context;
            video.VideoProcessorSetStreamFrameFormat(
                processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video.VideoProcessorSetStreamColorSpace(
                processor,
                0,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: STREAM_COLOR_SPACE,
                },
            );
            video.VideoProcessorSetOutputColorSpace(
                processor,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: OUTPUT_COLOR_SPACE,
                },
            );
            video.VideoProcessorSetStreamSourceRect(processor, 0, true, Some(&source));
            video.VideoProcessorSetStreamDestRect(processor, 0, true, Some(&target));
            // The whole window, black where the picture is not.
            video.VideoProcessorSetOutputTargetRect(processor, false, None);
            video.VideoProcessorSetOutputBackgroundColor(
                processor,
                false,
                &D3D11_VIDEO_COLOR {
                    Anonymous: D3D11_VIDEO_COLOR_0 {
                        RGBA: D3D11_VIDEO_COLOR_RGBA {
                            R: 0.0,
                            G: 0.0,
                            B: 0.0,
                            A: 1.0,
                        },
                    },
                },
            );
            // Pictures change in steps; no frame is made up between them.
            video.VideoProcessorSetStreamAutoProcessingMode(processor, 0, false);
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(input_view)),
                ..Default::default()
            };
            let mut streams = [stream];
            let result = video.VideoProcessorBlt(processor, &output_view, 0, &streams);
            std::mem::ManuallyDrop::drop(&mut streams[0].pInputSurface);
            result?;
            self.swap_chain.Present(0, DXGI_PRESENT(0)).ok()
        }
    }

    fn ensure_processor(&mut self, input: (u32, u32)) -> Result<()> {
        if let Some(processor) = &self.processor
            && processor.input == input
            && processor.output == self.size
        {
            return Ok(());
        }
        self.processor = None;
        self.output_view = None;
        self.input_views.clear();
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: 30,
                Denominator: 1,
            },
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            OutputWidth: self.size.0,
            OutputHeight: self.size.1,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // SAFETY: Direct3D calls on this renderer's device.
        unsafe {
            let enumerator = self.video_device.CreateVideoProcessorEnumerator(&content)?;
            let processor = self.video_device.CreateVideoProcessor(&enumerator, 0)?;
            self.processor = Some(Processor {
                enumerator,
                processor,
                input,
                output: self.size,
            });
        }
        Ok(())
    }

    fn input_view(
        &mut self,
        picture: &Picture,
        enumerator: &ID3D11VideoProcessorEnumerator,
    ) -> Result<ID3D11VideoProcessorInputView> {
        let key = picture.texture.as_raw() as usize;
        if let Some((_, _, view)) = self
            .input_views
            .iter()
            .find(|(texture, slice, _)| *texture == key && *slice == picture.slice)
        {
            return Ok(view.clone());
        }
        let description = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: picture.slice,
                },
            },
        };
        let mut view = None;
        // SAFETY: a view of the decoder's texture, on this renderer's device.
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                &picture.texture,
                enumerator,
                &description,
                Some(&mut view),
            )?;
        }
        let view = view.expect("input view");
        // A new texture array (the decoder started over): the old views go.
        if self.input_views.len() >= 32 {
            self.input_views.clear();
        }
        self.input_views.push((key, picture.slice, view.clone()));
        Ok(view)
    }

    fn output_view(&mut self) -> Result<ID3D11VideoProcessorOutputView> {
        if let Some(view) = &self.output_view {
            return Ok(view.clone());
        }
        let enumerator = &self.processor.as_ref().expect("processor").enumerator;
        let description = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut view = None;
        // SAFETY: a view of this renderer's back buffer. With the flip model,
        // Direct3D 11 keeps buffer 0 the one to draw into next.
        unsafe {
            let buffer: ID3D11Texture2D = self.swap_chain.GetBuffer(0)?;
            self.video_device.CreateVideoProcessorOutputView(
                &buffer,
                enumerator,
                &description,
                Some(&mut view),
            )?;
        }
        let view = view.expect("output view");
        self.output_view = Some(view.clone());
        Ok(view)
    }
}
