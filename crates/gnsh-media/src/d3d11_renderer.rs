use crate::video_decoder::{D3D11Frame, DecodedFrame};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::ffi::CStr;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP, D3D11_SRV_DIMENSION_TEXTURE2D,
    D3D11_SRV_DIMENSION_TEXTURE2DARRAY, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE, D3D11_BUFFER_DESC,
    D3D11_COMPARISON_NEVER, D3D11_CPU_ACCESS_WRITE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_FLAG, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
    D3D11_MAP_WRITE_DISCARD, D3D11_MAPPED_SUBRESOURCE, D3D11_SAMPLER_DESC, D3D11_SDK_VERSION,
    D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_SUBRESOURCE_DATA,
    D3D11_TEX2D_ARRAY_SRV, D3D11_TEX2D_SRV, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC, D3D11_VIEWPORT, D3D11CreateDevice, ID3D11Buffer,
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11PixelShader,
    ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D,
    ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8_UNORM,
    DXGI_FORMAT_R8G8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_FEATURE_PRESENT_ALLOW_TEARING, DXGI_PRESENT, DXGI_PRESENT_ALLOW_TEARING,
    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIDevice1,
    IDXGIFactory2, IDXGIFactory5, IDXGISwapChain1, IDXGISwapChain2,
};
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use windows::core::{BOOL, Interface as _, PCSTR, s};

const VIDEO_VS_HLSL: &str = r#"
cbuffer VideoParams : register(b0) {
    float4 pos_scale; // xy = scale, zw = offset
    float4 uv_rect;   // xy = active scale, zw = offset
    float4 uv_clamp;  // xy = luma max uv, zw = chroma max uv
};

struct VSOut {
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};

VSOut VSMain(uint vid : SV_VertexID) {
    float2 quad[4] = {
        float2(-1.0,  1.0),
        float2( 1.0,  1.0),
        float2(-1.0, -1.0),
        float2( 1.0, -1.0)
    };
    float2 uvs[4] = {
        float2(0.0, 0.0),
        float2(1.0, 0.0),
        float2(0.0, 1.0),
        float2(1.0, 1.0)
    };
    VSOut o;
    o.pos = float4(quad[vid] * pos_scale.xy + pos_scale.zw, 0.0, 1.0);
    o.uv = uvs[vid] * uv_rect.xy + uv_rect.zw;
    return o;
}
"#;

const NV12_ARRAY_PS_HLSL: &str = r#"
Texture2DArray<float> texY  : register(t0);
Texture2DArray<float2> texUV : register(t1);
SamplerState sampLinear : register(s0);
cbuffer VideoParams : register(b0) {
    float4 pos_scale;
    float4 uv_rect;
    float4 uv_clamp;
};

struct PSIn {
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};

float4 PSMain(PSIn i) : SV_TARGET {
    float y_raw = texY.Sample(sampLinear, float3(min(i.uv, uv_clamp.xy), 0.0));
    float2 uv_raw = texUV.Sample(sampLinear, float3(min(i.uv, uv_clamp.zw), 0.0));
    float y = (y_raw - 16.0 / 255.0) * (255.0 / 219.0);
    float cb = (uv_raw.x - 128.0 / 255.0) * (255.0 / 224.0);
    float cr = (uv_raw.y - 128.0 / 255.0) * (255.0 / 224.0);
    float r = y + 1.5748 * cr;
    float g = y - 0.187324 * cb - 0.468124 * cr;
    float b = y + 1.8556 * cb;
    return float4(saturate(float3(r, g, b)), 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct VideoParams {
    pos_scale: [f32; 4],
    uv_rect: [f32; 4],
    uv_clamp: [f32; 4],
}

impl Default for VideoParams {
    fn default() -> Self {
        Self {
            pos_scale: [1.0, 1.0, 0.0, 0.0],
            uv_rect: [1.0, 1.0, 0.0, 0.0],
            uv_clamp: [1.0, 1.0, 1.0, 1.0],
        }
    }
}

const YUV420_PS_HLSL: &str = r#"
Texture2D<float> texY : register(t0);
Texture2D<float> texU : register(t1);
Texture2D<float> texV : register(t2);
SamplerState sampLinear : register(s0);

struct PSIn {
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};

float4 PSMain(PSIn i) : SV_TARGET {
    float y_raw = texY.Sample(sampLinear, i.uv);
    float u_raw = texU.Sample(sampLinear, i.uv);
    float v_raw = texV.Sample(sampLinear, i.uv);
    float y = (y_raw - 16.0 / 255.0) * (255.0 / 219.0);
    float cb = (u_raw - 128.0 / 255.0) * (255.0 / 224.0);
    float cr = (v_raw - 128.0 / 255.0) * (255.0 / 224.0);
    float r = y + 1.5748 * cr;
    float g = y - 0.187324 * cb - 0.468124 * cr;
    float b = y + 1.8556 * cb;
    return float4(saturate(float3(r, g, b)), 1.0);
}
"#;

struct SwYuvTextures {
    width: u32,
    height: u32,
    y_tex: ID3D11Texture2D,
    u_tex: ID3D11Texture2D,
    v_tex: ID3D11Texture2D,
    y_srv: ID3D11ShaderResourceView,
    u_srv: ID3D11ShaderResourceView,
    v_srv: ID3D11ShaderResourceView,
}

#[derive(Default)]
struct Nv12SrvCache {
    texture_key: (usize, u32, u32),
    slices: HashMap<u32, (ID3D11ShaderResourceView, ID3D11ShaderResourceView)>,
}

impl Nv12SrvCache {
    fn clear(&mut self) {
        self.texture_key = (0, 0, 0);
        self.slices.clear();
    }
}

pub struct D3D11Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swapchain: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    swapchain_flags: u32,
    frame_latency_waitable: Option<HANDLE>,
    allow_tearing: bool,
    width: u32,
    height: u32,
    vs: ID3D11VertexShader,
    ps_nv12: ID3D11PixelShader,
    ps_yuv420: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    cb_params: ID3D11Buffer,
    nv12_srv_cache: Nv12SrvCache,
    last_video_params_key: Option<(u32, u32, u32, u32, u32, u32)>,
    sw_textures: Option<SwYuvTextures>,
}

impl D3D11Renderer {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        let w = width.max(1);
        let h = height.max(1);
        let (device, context) = create_d3d11_device()?;

        if let Ok(dxgi_dev1) = device.cast::<IDXGIDevice1>() {
            // SAFETY: Request 1-frame maximum GPU queue depth when supported by the DXGI driver.
            let _ = unsafe { dxgi_dev1.SetMaximumFrameLatency(1) };
        }

        let dxgi_device: IDXGIDevice = device.cast()?;
        let adapter = unsafe { dxgi_device.GetAdapter()? };
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent()? };

        let allow_tearing = unsafe {
            factory
                .cast::<IDXGIFactory5>()
                .ok()
                .and_then(|f5| {
                    let mut supported = BOOL(0);
                    f5.CheckFeatureSupport(
                        DXGI_FEATURE_PRESENT_ALLOW_TEARING,
                        (&raw mut supported).cast(),
                        std::mem::size_of::<BOOL>() as u32,
                    )
                    .ok()
                    .map(|_| supported.as_bool())
                })
                .unwrap_or(false)
        };

        let base_flags = if allow_tearing {
            DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32
        } else {
            0
        };
        let waitable_flags =
            base_flags | DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32;
        let (swapchain, swapchain_flags) = {
            let mut swapchain_desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w,
                Height: h,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: waitable_flags,
            };

            if let Ok(sc) = unsafe {
                factory.CreateSwapChainForHwnd(&device, hwnd, &swapchain_desc, None, None)
            } {
                (sc, waitable_flags)
            } else {
                swapchain_desc.Flags = base_flags;
                let sc = unsafe {
                    factory.CreateSwapChainForHwnd(&device, hwnd, &swapchain_desc, None, None)?
                };
                (sc, base_flags)
            }
        };

        let frame_latency_waitable =
            if (swapchain_flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32) != 0
            {
                swapchain.cast::<IDXGISwapChain2>().ok().and_then(|sc2| {
                    // SAFETY: swapchain was created with FRAME_LATENCY_WAITABLE_OBJECT; configuring
                    // latency before acquiring its waitable HANDLE keeps max present queue at 1 frame.
                    unsafe {
                        if sc2.SetMaximumFrameLatency(1).is_err() {
                            return None;
                        }
                        let handle = sc2.GetFrameLatencyWaitableObject();
                        if handle.is_invalid() {
                            None
                        } else {
                            Some(handle)
                        }
                    }
                })
            } else {
                None
            };

        let rtv = create_backbuffer_rtv(&device, &swapchain)?;

        let vs_blob = compile_hlsl(VIDEO_VS_HLSL, s!("VSMain"), s!("vs_5_0"))?;
        let ps_nv12_blob = compile_hlsl(NV12_ARRAY_PS_HLSL, s!("PSMain"), s!("ps_5_0"))?;
        let ps_yuv_blob = compile_hlsl(YUV420_PS_HLSL, s!("PSMain"), s!("ps_5_0"))?;

        let vs = unsafe {
            let mut out = None;
            device.CreateVertexShader(blob_slice(&vs_blob), None, Some(&mut out))?;
            out.context("CreateVertexShader returned None")?
        };
        let ps_nv12 = unsafe {
            let mut out = None;
            device.CreatePixelShader(blob_slice(&ps_nv12_blob), None, Some(&mut out))?;
            out.context("CreatePixelShader(NV12) returned None")?
        };
        let ps_yuv420 = unsafe {
            let mut out = None;
            device.CreatePixelShader(blob_slice(&ps_yuv_blob), None, Some(&mut out))?;
            out.context("CreatePixelShader(YUV420) returned None")?
        };

        let sampler_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let sampler = unsafe {
            let mut out = None;
            device.CreateSamplerState(&sampler_desc, Some(&mut out))?;
            out.context("CreateSamplerState returned None")?
        };

        let init_params = VideoParams::default();
        let cb_desc = D3D11_BUFFER_DESC {
            ByteWidth: std::mem::size_of::<VideoParams>() as u32,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let cb_sub = D3D11_SUBRESOURCE_DATA {
            pSysMem: (&init_params as *const VideoParams).cast(),
            ..Default::default()
        };
        let cb_params = unsafe {
            let mut out = None;
            device.CreateBuffer(&cb_desc, Some(&cb_sub), Some(&mut out))?;
            out.context("CreateBuffer returned None")?
        };

        Ok(Self {
            device,
            context,
            swapchain,
            rtv: Some(rtv),
            swapchain_flags,
            frame_latency_waitable,
            allow_tearing,
            width: w,
            height: h,
            vs,
            ps_nv12,
            ps_yuv420,
            sampler,
            cb_params,
            nv12_srv_cache: Nv12SrvCache::default(),
            last_video_params_key: None,
            sw_textures: None,
        })
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    pub fn backbuffer_rtv(&self) -> Option<&ID3D11RenderTargetView> {
        self.rtv.as_ref()
    }

    pub fn viewport_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let w = width.max(1);
        let h = height.max(1);
        if w == self.width && h == self.height {
            return Ok(());
        }
        unsafe {
            self.context.OMSetRenderTargets(None, None);
        }
        self.rtv = None;

        let flags = DXGI_SWAP_CHAIN_FLAG(self.swapchain_flags as i32);
        unsafe {
            self.swapchain
                .ResizeBuffers(0, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, flags)?;
        }
        self.rtv = Some(create_backbuffer_rtv(&self.device, &self.swapchain)?);
        self.width = w;
        self.height = h;
        self.last_video_params_key = None;
        Ok(())
    }

    /// Waits up to `timeout_ms` on DXGI's waitable frame-latency object to prevent swapchain queuing.
    pub fn wait_for_frame_latency(&self, timeout_ms: u32) {
        if let Some(handle) = self.frame_latency_waitable {
            // SAFETY: handle is an owned DXGI swapchain latency waitable event opened in new().
            unsafe {
                let _ = WaitForSingleObjectEx(handle, timeout_ms, false);
            }
        }
    }

    /// Releases cached video SRVs and constant-buffer keys (called on stream shutdown).
    pub fn clear_video_cache(&mut self) {
        self.nv12_srv_cache.clear();
        self.last_video_params_key = None;
    }

    pub fn begin_frame(&self, clear_rgba: [f32; 4]) {
        self.bind_backbuffer();
        if let Some(rtv) = &self.rtv {
            unsafe {
                self.context.ClearRenderTargetView(rtv, &clear_rgba);
            }
        }
    }

    pub fn bind_backbuffer(&self) {
        if let Some(rtv) = &self.rtv {
            let vp = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: self.width as f32,
                Height: self.height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            unsafe {
                self.context.RSSetViewports(Some(&[vp]));
                self.context
                    .OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            }
        }
    }

    pub fn draw_video_frame(&mut self, frame: &DecodedFrame) -> Result<()> {
        self.bind_backbuffer();

        let (fw, fh, tw, th) = match frame {
            DecodedFrame::D3D11(f) => {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                unsafe {
                    f.texture.GetDesc(&mut desc);
                }
                (f.width, f.height, desc.Width, desc.Height)
            }
            DecodedFrame::SoftwareYuv420 { width, height, .. } => {
                (*width, *height, *width, *height)
            }
        };
        let key = (fw, fh, tw, th, self.width, self.height);
        if self.last_video_params_key != Some(key) {
            let params = compute_video_params(fw, fh, tw, th, self.width, self.height);
            unsafe {
                self.context.UpdateSubresource(
                    &self.cb_params,
                    0,
                    None,
                    (&params as *const VideoParams).cast(),
                    0,
                    0,
                );
            }
            self.last_video_params_key = Some(key);
        }
        unsafe {
            self.context.IASetInputLayout(None);
            self.context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            self.context.VSSetShader(&self.vs, None);
            let cb = [Some(self.cb_params.clone())];
            self.context.VSSetConstantBuffers(0, Some(&cb));
            self.context.PSSetConstantBuffers(0, Some(&cb));
            self.context
                .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
        }

        match frame {
            DecodedFrame::D3D11(d3d_frame) => self.draw_d3d11_nv12(d3d_frame, tw, th)?,
            DecodedFrame::SoftwareYuv420 {
                y,
                u,
                v,
                y_stride,
                u_stride,
                v_stride,
                width,
                height,
                ..
            } => self.draw_sw_yuv420(y, u, v, *y_stride, *u_stride, *v_stride, *width, *height)?,
        }

        unsafe {
            self.context
                .PSSetShaderResources(0, Some(&[None, None, None]));
        }
        Ok(())
    }

    fn draw_d3d11_nv12(&mut self, frame: &D3D11Frame, tex_w: u32, tex_h: u32) -> Result<()> {
        let texture_key = (frame.texture.as_raw() as usize, tex_w, tex_h);
        if self.nv12_srv_cache.texture_key != texture_key {
            self.nv12_srv_cache.slices.clear();
            self.nv12_srv_cache.texture_key = texture_key;
        }

        let slice_index = frame.slice_index;
        let (y_srv, uv_srv) = match self.nv12_srv_cache.slices.entry(slice_index) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let (y, uv) = entry.get();
                (y.clone(), uv.clone())
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let y_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                    Format: DXGI_FORMAT_R8_UNORM,
                    ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2DARRAY,
                    Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2DArray: D3D11_TEX2D_ARRAY_SRV {
                            MostDetailedMip: 0,
                            MipLevels: 1,
                            FirstArraySlice: slice_index,
                            ArraySize: 1,
                        },
                    },
                };
                let uv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                    Format: DXGI_FORMAT_R8G8_UNORM,
                    ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2DARRAY,
                    Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2DArray: D3D11_TEX2D_ARRAY_SRV {
                            MostDetailedMip: 0,
                            MipLevels: 1,
                            FirstArraySlice: slice_index,
                            ArraySize: 1,
                        },
                    },
                };

                let y_srv = unsafe {
                    let mut out = None;
                    self.device.CreateShaderResourceView(
                        &frame.texture,
                        Some(&y_desc),
                        Some(&mut out),
                    )?;
                    out.context("CreateShaderResourceView(Y) returned None")?
                };
                let uv_srv = unsafe {
                    let mut out = None;
                    self.device.CreateShaderResourceView(
                        &frame.texture,
                        Some(&uv_desc),
                        Some(&mut out),
                    )?;
                    out.context("CreateShaderResourceView(UV) returned None")?
                };
                let (y, uv) = entry.insert((y_srv, uv_srv));
                (y.clone(), uv.clone())
            }
        };

        unsafe {
            self.context.PSSetShader(&self.ps_nv12, None);
            self.context
                .PSSetShaderResources(0, Some(&[Some(y_srv), Some(uv_srv)]));
            self.context.Draw(4, 0);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_sw_yuv420(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        y_stride: usize,
        u_stride: usize,
        v_stride: usize,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let recreate = self
            .sw_textures
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height);
        if recreate {
            self.sw_textures = Some(create_sw_yuv_textures(&self.device, width, height)?);
        }

        let texs = self.sw_textures.as_ref().expect("sw_textures initialized");
        let cw = width.div_ceil(2);
        let ch = height.div_ceil(2);
        upload_plane(&self.context, &texs.y_tex, y, y_stride, width, height)?;
        upload_plane(&self.context, &texs.u_tex, u, u_stride, cw, ch)?;
        upload_plane(&self.context, &texs.v_tex, v, v_stride, cw, ch)?;

        unsafe {
            self.context.PSSetShader(&self.ps_yuv420, None);
            self.context.PSSetShaderResources(
                0,
                Some(&[
                    Some(texs.y_srv.clone()),
                    Some(texs.u_srv.clone()),
                    Some(texs.v_srv.clone()),
                ]),
            );
            self.context.Draw(4, 0);
        }
        Ok(())
    }

    pub fn present(&self, vsync: bool) -> Result<()> {
        let (interval, flags) = if vsync {
            (1, DXGI_PRESENT(0))
        } else if self.allow_tearing {
            (0, DXGI_PRESENT_ALLOW_TEARING)
        } else {
            (0, DXGI_PRESENT(0))
        };
        unsafe { self.swapchain.Present(interval, flags).ok()? };
        Ok(())
    }
}

impl Drop for D3D11Renderer {
    fn drop(&mut self) {
        self.nv12_srv_cache.clear();
        if let Some(handle) = self.frame_latency_waitable.take()
            && !handle.is_invalid()
        {
            // SAFETY: handle was returned by IDXGISwapChain2::GetFrameLatencyWaitableObject.
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }
}

fn create_d3d11_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
    let try_flags = [
        D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    ];

    for flags in try_flags {
        let mut dev = None;
        let mut ctx = None;
        let res = unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(flags.0),
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut dev),
                None,
                Some(&mut ctx),
            )
        };
        if res.is_ok()
            && let (Some(d), Some(c)) = (dev, ctx)
        {
            if let Ok(mt) = d.cast::<ID3D11Multithread>() {
                unsafe {
                    let _ = mt.SetMultithreadProtected(true);
                }
            }
            return Ok((d, c));
        }
    }

    bail!("D3D11CreateDevice failed for hardware adapter")
}

fn create_backbuffer_rtv(
    device: &ID3D11Device,
    swapchain: &IDXGISwapChain1,
) -> Result<ID3D11RenderTargetView> {
    let backbuffer: ID3D11Texture2D = unsafe { swapchain.GetBuffer(0)? };
    let mut rtv = None;
    unsafe {
        device.CreateRenderTargetView(&backbuffer, None, Some(&mut rtv))?;
    }
    rtv.context("CreateRenderTargetView returned None")
}

fn create_sw_yuv_textures(device: &ID3D11Device, width: u32, height: u32) -> Result<SwYuvTextures> {
    let cw = width.div_ceil(2);
    let ch = height.div_ceil(2);
    let (y_tex, y_srv) = create_r8_dynamic_tex(device, width, height)?;
    let (u_tex, u_srv) = create_r8_dynamic_tex(device, cw, ch)?;
    let (v_tex, v_srv) = create_r8_dynamic_tex(device, cw, ch)?;
    Ok(SwYuvTextures {
        width,
        height,
        y_tex,
        u_tex,
        v_tex,
        y_srv,
        u_srv,
        v_srv,
    })
}

fn create_r8_dynamic_tex(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<(ID3D11Texture2D, ID3D11ShaderResourceView)> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width.max(1),
        Height: height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
    };
    let tex = unsafe {
        let mut out = None;
        device.CreateTexture2D(&desc, None, Some(&mut out))?;
        out.context("CreateTexture2D(R8) returned None")?
    };
    let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_R8_UNORM,
        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
            },
        },
    };
    let srv = unsafe {
        let mut out = None;
        device.CreateShaderResourceView(&tex, Some(&srv_desc), Some(&mut out))?;
        out.context("CreateShaderResourceView(R8) returned None")?
    };
    Ok((tex, srv))
}

fn upload_plane(
    context: &ID3D11DeviceContext,
    tex: &ID3D11Texture2D,
    data: &[u8],
    src_stride: usize,
    width: u32,
    height: u32,
) -> Result<()> {
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        context.Map(tex, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
        let dst_ptr = mapped.pData.cast::<u8>();
        let dst_stride = mapped.RowPitch as usize;
        let copy_w = width as usize;
        for row in 0..height as usize {
            let src_start = row * src_stride;
            let src_end = src_start + copy_w;
            if src_end <= data.len() {
                std::ptr::copy_nonoverlapping(
                    data[src_start..src_end].as_ptr(),
                    dst_ptr.add(row * dst_stride),
                    copy_w,
                );
            }
        }
        context.Unmap(tex, 0);
    }
    Ok(())
}

fn aspect_scale(frame_w: u32, frame_h: u32, view_w: u32, view_h: u32) -> [f32; 4] {
    if frame_w == 0 || frame_h == 0 || view_w == 0 || view_h == 0 {
        return [1.0, 1.0, 0.0, 0.0];
    }
    let frame_aspect = frame_w as f32 / frame_h as f32;
    let view_aspect = view_w as f32 / view_h as f32;
    if view_aspect > frame_aspect {
        [frame_aspect / view_aspect, 1.0, 0.0, 0.0]
    } else {
        [1.0, view_aspect / frame_aspect, 0.0, 0.0]
    }
}

fn compute_video_params(
    frame_w: u32,
    frame_h: u32,
    tex_w: u32,
    tex_h: u32,
    view_w: u32,
    view_h: u32,
) -> VideoParams {
    let pos_scale = aspect_scale(frame_w, frame_h, view_w, view_h);
    let tw = tex_w.max(frame_w).max(1) as f32;
    let th = tex_h.max(frame_h).max(1) as f32;
    let fw = frame_w.max(1) as f32;
    let fh = frame_h.max(1) as f32;
    VideoParams {
        pos_scale,
        uv_rect: [fw / tw, fh / th, 0.0, 0.0],
        uv_clamp: [
            (fw - 0.5).max(0.0) / tw,
            (fh - 0.5).max(0.0) / th,
            (fw - 1.0).max(0.0) / tw,
            (fh - 1.0).max(0.0) / th,
        ],
    }
}

pub fn compile_hlsl(source: &str, entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut code = None;
    let mut err_blob = None;
    let res = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            None,
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut code,
            Some(&mut err_blob),
        )
    };
    if let Err(e) = res {
        let msg = err_blob
            .as_ref()
            .map(|b| unsafe {
                CStr::from_ptr(b.GetBufferPointer().cast())
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_default();
        bail!("HLSL compile error ({e}): {msg}");
    }
    code.context("D3DCompile returned None")
}

pub fn blob_slice(blob: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_video_hlsl_shaders() {
        let vs = compile_hlsl(VIDEO_VS_HLSL, s!("VSMain"), s!("vs_5_0")).expect("VS compiles");
        let ps_nv12 =
            compile_hlsl(NV12_ARRAY_PS_HLSL, s!("PSMain"), s!("ps_5_0")).expect("NV12 PS compiles");
        let ps_yuv =
            compile_hlsl(YUV420_PS_HLSL, s!("PSMain"), s!("ps_5_0")).expect("YUV420 PS compiles");

        assert!(!blob_slice(&vs).is_empty());
        assert!(!blob_slice(&ps_nv12).is_empty());
        assert!(!blob_slice(&ps_yuv).is_empty());
    }

    #[test]
    fn computes_aspect_letterbox_scale() {
        let exact = aspect_scale(1920, 1080, 1920, 1080);
        assert!((exact[0] - 1.0).abs() < 1e-5 && (exact[1] - 1.0).abs() < 1e-5);

        let wide = aspect_scale(1920, 1080, 2560, 1080);
        assert!(wide[0] < 1.0 && (wide[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn computes_padded_surface_uv_crop() {
        let hevc = compute_video_params(1920, 1080, 1920, 1152, 1920, 1080);
        assert!((hevc.uv_rect[0] - 1.0).abs() < 1e-6);
        assert!((hevc.uv_rect[1] - (1080.0 / 1152.0)).abs() < 1e-6);
        assert!(hevc.uv_clamp[1] < hevc.uv_rect[1]);
        assert!(hevc.uv_clamp[3] < hevc.uv_clamp[1]);

        let h264 = compute_video_params(1920, 1080, 1920, 1088, 1920, 1080);
        assert!((h264.uv_rect[1] - (1080.0 / 1088.0)).abs() < 1e-6);
    }
}
