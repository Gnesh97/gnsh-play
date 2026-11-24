use crate::d3d11_renderer::{
    FSR_EASU_PS_HLSL, FSR_RCAS_PS_HLSL, FsrParams, HLSL_COLOR_CAS_HELPERS, NV12_ARRAY_PS_BODY,
    P010_ARRAY_PS_BODY, VIDEO_VS_HLSL, VideoParams, YUV420_PS_BODY, aspect_scale, blob_slice,
    compile_hlsl, compute_rcas_bounds, compute_video_params, should_run_fsr_easu,
    unpack_nv12_chroma, unpack_p010_to_yuv8,
};
use crate::renderer::{
    ScaleMode, UiMeshDraw, UiOverlayRenderer, UiTextureWrap, VideoFilterMode, VideoRenderOptions,
    VideoRenderer,
};
use crate::video_decoder::{AvFrameGuard, D3D12Frame, DecodedFrame};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_FEATURE_LEVEL_11_0, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
    D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D12::{
    D3D_ROOT_SIGNATURE_VERSION_1_0, D3D12_BLEND_DESC, D3D12_BLEND_INV_DEST_ALPHA,
    D3D12_BLEND_INV_SRC_ALPHA, D3D12_BLEND_ONE, D3D12_BLEND_OP_ADD, D3D12_BOX, D3D12_CLEAR_VALUE,
    D3D12_CLEAR_VALUE_0, D3D12_COLOR_WRITE_ENABLE_ALL, D3D12_COMMAND_LIST_TYPE_DIRECT,
    D3D12_COMMAND_QUEUE_DESC, D3D12_COMMAND_QUEUE_FLAG_NONE, D3D12_COMPARISON_FUNC_ALWAYS,
    D3D12_COMPARISON_FUNC_NEVER, D3D12_CONSTANT_BUFFER_DATA_PLACEMENT_ALIGNMENT,
    D3D12_CPU_DESCRIPTOR_HANDLE, D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_CULL_MODE_NONE,
    D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING, D3D12_DEPTH_STENCIL_DESC, D3D12_DEPTH_STENCILOP_DESC,
    D3D12_DEPTH_WRITE_MASK_ZERO, D3D12_DESCRIPTOR_HEAP_DESC, D3D12_DESCRIPTOR_HEAP_FLAG_NONE,
    D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
    D3D12_DESCRIPTOR_HEAP_TYPE_RTV, D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER, D3D12_DESCRIPTOR_RANGE,
    D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER, D3D12_DESCRIPTOR_RANGE_TYPE_SRV, D3D12_FENCE_FLAG_NONE,
    D3D12_FILL_MODE_SOLID, D3D12_FILTER_MIN_MAG_MIP_LINEAR, D3D12_FILTER_MIN_MAG_MIP_POINT,
    D3D12_GPU_DESCRIPTOR_HANDLE, D3D12_GRAPHICS_PIPELINE_STATE_DESC, D3D12_HEAP_FLAG_NONE,
    D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT, D3D12_HEAP_TYPE_UPLOAD,
    D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_DISABLED, D3D12_INDEX_BUFFER_VIEW,
    D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA, D3D12_INPUT_ELEMENT_DESC, D3D12_INPUT_LAYOUT_DESC,
    D3D12_LOGIC_OP_NOOP, D3D12_MEMORY_POOL_UNKNOWN, D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
    D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE, D3D12_RASTERIZER_DESC, D3D12_RENDER_TARGET_BLEND_DESC,
    D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_DESC,
    D3D12_RESOURCE_DIMENSION_BUFFER, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
    D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON,
    D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_GENERIC_READ,
    D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE, D3D12_RESOURCE_STATE_PRESENT,
    D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATES, D3D12_RESOURCE_TRANSITION_BARRIER,
    D3D12_ROOT_CONSTANTS, D3D12_ROOT_DESCRIPTOR, D3D12_ROOT_DESCRIPTOR_TABLE, D3D12_ROOT_PARAMETER,
    D3D12_ROOT_PARAMETER_0, D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
    D3D12_ROOT_PARAMETER_TYPE_CBV, D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
    D3D12_ROOT_SIGNATURE_DESC, D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
    D3D12_ROOT_SIGNATURE_FLAG_NONE, D3D12_SAMPLER_DESC, D3D12_SHADER_BYTECODE,
    D3D12_SHADER_RESOURCE_VIEW_DESC, D3D12_SHADER_RESOURCE_VIEW_DESC_0,
    D3D12_SHADER_VISIBILITY_ALL, D3D12_SHADER_VISIBILITY_PIXEL, D3D12_SHADER_VISIBILITY_VERTEX,
    D3D12_SRV_DIMENSION_TEXTURE2D, D3D12_SRV_DIMENSION_TEXTURE2DARRAY,
    D3D12_STATIC_BORDER_COLOR_TRANSPARENT_BLACK, D3D12_STATIC_SAMPLER_DESC, D3D12_STENCIL_OP_KEEP,
    D3D12_SUBRESOURCE_FOOTPRINT, D3D12_TEX2D_ARRAY_SRV, D3D12_TEX2D_SRV,
    D3D12_TEXTURE_ADDRESS_MODE_CLAMP, D3D12_TEXTURE_ADDRESS_MODE_MIRROR,
    D3D12_TEXTURE_ADDRESS_MODE_WRAP, D3D12_TEXTURE_COPY_LOCATION, D3D12_TEXTURE_COPY_LOCATION_0,
    D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT, D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
    D3D12_TEXTURE_DATA_PITCH_ALIGNMENT, D3D12_TEXTURE_DATA_PLACEMENT_ALIGNMENT,
    D3D12_TEXTURE_LAYOUT_ROW_MAJOR, D3D12_TEXTURE_LAYOUT_UNKNOWN, D3D12_VERTEX_BUFFER_VIEW,
    D3D12_VIEWPORT, D3D12CreateDevice, D3D12SerializeRootSignature, ID3D12CommandAllocator,
    ID3D12CommandList, ID3D12CommandQueue, ID3D12DescriptorHeap, ID3D12Device, ID3D12Fence,
    ID3D12GraphicsCommandList, ID3D12PipelineState, ID3D12Resource, ID3D12RootSignature,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_P010, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM,
    DXGI_FORMAT_R32_UINT, DXGI_FORMAT_R32G32_FLOAT, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_SOFTWARE,
    DXGI_FEATURE_PRESENT_ALLOW_TEARING, DXGI_PRESENT, DXGI_PRESENT_ALLOW_TEARING,
    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory4, IDXGIFactory5,
    IDXGIOutput6, IDXGISwapChain3,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObjectEx};
use windows::core::{BOOL, IUnknown, Interface as _, s};

const FRAME_COUNT: usize = 2;
const RTV_SLOT_FSR_SRC: u32 = FRAME_COUNT as u32;
const RTV_SLOT_FSR_EASU: u32 = (FRAME_COUNT as u32) + 1;
const TOTAL_RTV_SLOTS: u32 = (FRAME_COUNT as u32) + 2;

const UI_TEXTURE_SLOTS: u32 = 512;
const TRANSIENT_SRV_SLOTS_PER_FRAME: u32 = 1024;
const TOTAL_SRV_SLOTS: u32 =
    UI_TEXTURE_SLOTS + (FRAME_COUNT as u32) * TRANSIENT_SRV_SLOTS_PER_FRAME;
const UPLOAD_ARENA_BYTES: usize = 16 * 1024 * 1024;
const EGUI_VERTEX_STRIDE: u32 = 20;

const UI_VS_HLSL: &str = r#"
cbuffer ScreenParams : register(b0) {
    float2 screen_size;
    float hdr_paper_white_nits;
    float _pad;
};

struct VSIn {
    float2 pos   : POSITION;
    float2 uv    : TEXCOORD0;
    float4 color : COLOR0;
};

struct VSOut {
    float4 pos   : SV_POSITION;
    float2 uv    : TEXCOORD0;
    float4 color : COLOR0;
};

VSOut VSMain(VSIn input) {
    VSOut output;
    output.pos = float4(
        input.pos.x * 2.0 / screen_size.x - 1.0,
        1.0 - input.pos.y * 2.0 / screen_size.y,
        0.0,
        1.0
    );
    output.uv = input.uv;
    output.color = input.color;
    return output;
}
"#;

const UI_PS_HLSL: &str = r#"
Texture2D tex0 : register(t0);
SamplerState samp0 : register(s0);
cbuffer ScreenParams : register(b0) {
    float2 screen_size;
    float hdr_paper_white_nits;
    float _pad;
};

struct PSIn {
    float4 pos   : SV_POSITION;
    float2 uv    : TEXCOORD0;
    float4 color : COLOR0;
};

float3 srgb_to_linear_egui(float3 c) {
    float3 lo = c / 12.92;
    float3 hi = pow(max((c + 0.055) / 1.055, 0.0), 2.4);
    return (c <= 0.04045) ? lo : hi;
}

float3 linear_to_pq_egui(float3 lin) {
    float3 ym = pow(saturate(lin), 0.1593017578125);
    return pow((0.8359375 + 18.8515625 * ym) / (1.0 + 18.6875 * ym), 78.84375);
}

float4 PSMain(PSIn input) : SV_TARGET {
    float4 col = input.color * tex0.Sample(samp0, input.uv);
    if (hdr_paper_white_nits > 0.5) {
        float a = col.a;
        float3 srgb = (a > 1e-4) ? saturate(col.rgb / a) : float3(0.0, 0.0, 0.0);
        float3 lin709 = srgb_to_linear_egui(srgb);
        float3 lin2020 = float3(
            0.627404 * lin709.r + 0.329283 * lin709.g + 0.043313 * lin709.b,
            0.069097 * lin709.r + 0.919540 * lin709.g + 0.011363 * lin709.b,
            0.016391 * lin709.r + 0.088013 * lin709.g + 0.895596 * lin709.b
        );
        float3 pq = linear_to_pq_egui(lin2020 * (hdr_paper_white_nits / 10000.0));
        return float4(pq * a, a);
    }
    return col;
}
"#;

struct CompiledShaders {
    video_vs: ID3DBlob,
    ps_nv12: ID3DBlob,
    ps_p010: ID3DBlob,
    ps_yuv420: ID3DBlob,
    ps_fsr_easu: ID3DBlob,
    ps_fsr_rcas: ID3DBlob,
    ui_vs: ID3DBlob,
    ui_ps: ID3DBlob,
}

struct PipelineFormatSet {
    ps_nv12: ID3D12PipelineState,
    ps_p010: ID3D12PipelineState,
    ps_yuv420: ID3D12PipelineState,
    ps_fsr_easu: ID3D12PipelineState,
    ps_fsr_rcas: ID3D12PipelineState,
    ui: ID3D12PipelineState,
}

#[derive(Clone, Copy)]
struct RenderTargetTex {
    width: u32,
    height: u32,
    rtv_slot: u32,
}

struct OwnedRenderTarget {
    meta: RenderTargetTex,
    resource: ID3D12Resource,
    state: D3D12_RESOURCE_STATES,
}

struct SwPlane {
    resource: ID3D12Resource,
    state: D3D12_RESOURCE_STATES,
}

struct SwYuvTextures {
    width: u32,
    height: u32,
    y: SwPlane,
    u: SwPlane,
    v: SwPlane,
}

struct UiTextureEntry {
    resource: ID3D12Resource,
    state: D3D12_RESOURCE_STATES,
    width: u32,
    height: u32,
    srv_slot: u32,
}

struct UploadArena {
    resource: ID3D12Resource,
    cpu_ptr: *mut u8,
    gpu_addr: u64,
    capacity: usize,
    cursor: usize,
}

// SAFETY: UploadArena points into persistently mapped coherent D3D12 upload heap memory owned exclusively
// by the FrameSlot and protected by `Mutex<RendererState>`.
unsafe impl Send for UploadArena {}

impl UploadArena {
    fn new(device: &ID3D12Device, capacity: usize) -> Result<Self> {
        let resource = create_upload_buffer(device, capacity)?;
        let mut ptr = std::ptr::null_mut();
        // SAFETY: UPLOAD heap resource supports persistent CPU read-range(0..0) Map until Drop.
        unsafe {
            resource.Map(0, None, Some(&mut ptr))?;
        }
        let gpu_addr = unsafe { resource.GetGPUVirtualAddress() };
        Ok(Self {
            resource,
            cpu_ptr: ptr.cast::<u8>(),
            gpu_addr,
            capacity,
            cursor: 0,
        })
    }

    fn alloc(&mut self, bytes: usize, alignment: usize) -> Option<(u64, *mut u8, usize)> {
        let offset = align_up(self.cursor, alignment);
        let end = offset.checked_add(bytes)?;
        if end > self.capacity {
            return None;
        }
        self.cursor = end;
        // SAFETY: `offset..end` is inside the mapped `[0, capacity)` range.
        Some((
            self.gpu_addr + offset as u64,
            unsafe { self.cpu_ptr.add(offset) },
            offset,
        ))
    }
}

impl Drop for UploadArena {
    fn drop(&mut self) {
        // SAFETY: `resource` was persistently mapped in `UploadArena::new`.
        unsafe {
            self.resource.Unmap(0, None);
        }
    }
}

struct FrameSlot {
    allocator: ID3D12CommandAllocator,
    upload: UploadArena,
    transient_srv_cursor: u32,
    fence_value: u64,
    pinned_frames: Vec<Arc<AvFrameGuard>>,
    retired_resources: Vec<ID3D12Resource>,
    retired_ui_slots: Vec<u32>,
    overflow_uploads: Vec<ID3D12Resource>,
}

struct RendererState {
    backbuffers: [Option<ID3D12Resource>; FRAME_COUNT],
    frames: [FrameSlot; FRAME_COUNT],
    active_slot: usize,
    frame_open: bool,
    next_fence_value: u64,
    pending_decode_waits: Vec<(ID3D12Fence, u64)>,
    sw_textures: Option<SwYuvTextures>,
    fsr_src_rt: Option<OwnedRenderTarget>,
    fsr_easu_rt: Option<OwnedRenderTarget>,
    ui_textures: HashMap<u64, UiTextureEntry>,
    ui_next_slot: u32,
    ui_free_slots: Vec<u32>,
    hdr_output_active: bool,
    swapchain_format: DXGI_FORMAT,
    width: u32,
    height: u32,
}

pub struct D3D12Renderer {
    device: ID3D12Device,
    command_queue: ID3D12CommandQueue,
    swapchain: IDXGISwapChain3,
    swapchain_flags: u32,
    frame_latency_waitable: Option<HANDLE>,
    allow_tearing: bool,
    rtv_heap: ID3D12DescriptorHeap,
    rtv_inc: u32,
    srv_heap: ID3D12DescriptorHeap,
    srv_inc: u32,
    sampler_heap: ID3D12DescriptorHeap,
    sampler_inc: u32,
    cmd_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_event: HANDLE,
    video_root_sig: ID3D12RootSignature,
    ui_root_sig: ID3D12RootSignature,
    pso_sdr: PipelineFormatSet,
    pso_r10: PipelineFormatSet,
    state: Mutex<RendererState>,
}

// SAFETY: COM interfaces wrapped in D3D12Renderer are agile/thread-safe (`Send + Sync` in windows-rs),
// win32 waitable HANDLEs are guarded by the OS kernel, and mutable recording state lives inside Mutex.
unsafe impl Send for D3D12Renderer {}
unsafe impl Sync for D3D12Renderer {}

impl D3D12Renderer {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        let w = width.max(1);
        let h = height.max(1);
        let (factory, device) = create_d3d12_device()?;

        let queue_desc = D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
            Priority: 0,
            Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        };
        let command_queue: ID3D12CommandQueue = unsafe {
            device
                .CreateCommandQueue(&queue_desc)
                .context("CreateCommandQueue(DIRECT) failed")?
        };

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
        let (swapchain1, swapchain_flags) = {
            let mut desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w,
                Height: h,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: FRAME_COUNT as u32,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: waitable_flags,
            };
            if let Ok(sc) =
                unsafe { factory.CreateSwapChainForHwnd(&command_queue, hwnd, &desc, None, None) }
            {
                (sc, waitable_flags)
            } else {
                desc.Flags = base_flags;
                let sc = unsafe {
                    factory
                        .CreateSwapChainForHwnd(&command_queue, hwnd, &desc, None, None)
                        .context("CreateSwapChainForHwnd(D3D12) failed")?
                };
                (sc, base_flags)
            }
        };

        let swapchain: IDXGISwapChain3 = swapchain1
            .cast()
            .context("DXGI swapchain does not implement IDXGISwapChain3")?;

        let frame_latency_waitable =
            if (swapchain_flags & DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32) != 0
            {
                // SAFETY: swapchain created with FRAME_LATENCY_WAITABLE_OBJECT; limit queued frames to 1.
                unsafe {
                    if swapchain.SetMaximumFrameLatency(1).is_ok() {
                        let handle = swapchain.GetFrameLatencyWaitableObject();
                        (!handle.is_invalid()).then_some(handle)
                    } else {
                        None
                    }
                }
            } else {
                None
            };

        let rtv_heap: ID3D12DescriptorHeap = unsafe {
            device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                    NumDescriptors: TOTAL_RTV_SLOTS,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_NONE,
                    NodeMask: 0,
                })
                .context("CreateDescriptorHeap(RTV) failed")?
        };
        let rtv_inc =
            unsafe { device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) };

        let srv_heap: ID3D12DescriptorHeap = unsafe {
            device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                    NumDescriptors: TOTAL_SRV_SLOTS,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    NodeMask: 0,
                })
                .context("CreateDescriptorHeap(CBV_SRV_UAV) failed")?
        };
        let srv_inc = unsafe {
            device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV)
        };

        let sampler_heap: ID3D12DescriptorHeap = unsafe {
            device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER,
                    NumDescriptors: 6,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    NodeMask: 0,
                })
                .context("CreateDescriptorHeap(SAMPLER) failed")?
        };
        let sampler_inc =
            unsafe { device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER) };
        populate_ui_samplers(&device, &sampler_heap, sampler_inc);

        let shaders = compile_all_shaders()?;
        let (video_root_sig, ui_root_sig) = create_root_signatures(&device)?;
        let pso_sdr = create_pipeline_set(
            &device,
            &video_root_sig,
            &ui_root_sig,
            &shaders,
            DXGI_FORMAT_B8G8R8A8_UNORM,
        )?;
        let pso_r10 = create_pipeline_set(
            &device,
            &video_root_sig,
            &ui_root_sig,
            &shaders,
            DXGI_FORMAT_R10G10B10A2_UNORM,
        )?;

        let frame0 = create_frame_slot(&device)?;
        let frame1 = create_frame_slot(&device)?;
        let cmd_list: ID3D12GraphicsCommandList = unsafe {
            device
                .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &frame0.allocator, None)
                .context("CreateCommandList(DIRECT) failed")?
        };
        unsafe {
            cmd_list
                .Close()
                .context("Initial command list Close failed")?;
        }

        let fence: ID3D12Fence = unsafe {
            device
                .CreateFence(0, D3D12_FENCE_FLAG_NONE)
                .context("CreateFence failed")?
        };
        let fence_event = unsafe {
            CreateEventW(None, false, false, None).context("CreateEventW(D3D12 fence) failed")?
        };

        let backbuffers = create_backbuffers(&device, &swapchain, &rtv_heap, rtv_inc)?;

        Ok(Self {
            device,
            command_queue,
            swapchain,
            swapchain_flags,
            frame_latency_waitable,
            allow_tearing,
            rtv_heap,
            rtv_inc,
            srv_heap,
            srv_inc,
            sampler_heap,
            sampler_inc,
            cmd_list,
            fence,
            fence_event,
            video_root_sig,
            ui_root_sig,
            pso_sdr,
            pso_r10,
            state: Mutex::new(RendererState {
                backbuffers,
                frames: [frame0, frame1],
                active_slot: 0,
                frame_open: false,
                next_fence_value: 0,
                pending_decode_waits: Vec::new(),
                sw_textures: None,
                fsr_src_rt: None,
                fsr_easu_rt: None,
                ui_textures: HashMap::new(),
                ui_next_slot: 0,
                ui_free_slots: Vec::new(),
                hdr_output_active: false,
                swapchain_format: DXGI_FORMAT_B8G8R8A8_UNORM,
                width: w,
                height: h,
            }),
        })
    }

    pub fn device(&self) -> &ID3D12Device {
        &self.device
    }

    pub fn viewport_size(&self) -> (u32, u32) {
        let state = self.state.lock().expect("d3d12 state mutex poisoned");
        (state.width, state.height)
    }

    pub fn is_display_hdr(&self) -> bool {
        let Ok(output) = (unsafe { self.swapchain.GetContainingOutput() }) else {
            return false;
        };
        let Ok(output6) = output.cast::<IDXGIOutput6>() else {
            return false;
        };
        let Ok(desc) = (unsafe { output6.GetDesc1() }) else {
            return false;
        };
        desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
    }

    pub fn is_hdr_output_active(&self) -> bool {
        self.state
            .lock()
            .expect("d3d12 state mutex poisoned")
            .hdr_output_active
    }

    pub fn wait_for_frame_latency(&self, timeout_ms: u32) {
        if let Some(handle) = self.frame_latency_waitable {
            // SAFETY: handle is an owned DXGI waitable object created in `D3D12Renderer::new`.
            unsafe {
                let _ = WaitForSingleObjectEx(handle, timeout_ms, false);
            }
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let w = width.max(1);
        let h = height.max(1);
        let state = self.state.get_mut().expect("d3d12 state mutex poisoned");
        if w == state.width && h == state.height {
            return Ok(());
        }

        close_open_cmd_list_if_needed(&self.cmd_list, state);
        flush_gpu(&self.command_queue, &self.fence, self.fence_event, state)?;
        for bb in &mut state.backbuffers {
            *bb = None;
        }

        let flags = DXGI_SWAP_CHAIN_FLAG(self.swapchain_flags as i32);
        unsafe {
            self.swapchain
                .ResizeBuffers(FRAME_COUNT as u32, w, h, state.swapchain_format, flags)
                .context("D3D12 ResizeBuffers failed")?;
        }
        state.backbuffers =
            create_backbuffers(&self.device, &self.swapchain, &self.rtv_heap, self.rtv_inc)?;
        state.width = w;
        state.height = h;
        state.fsr_easu_rt = None;
        Ok(())
    }

    pub fn set_hdr_output(&mut self, enable: bool) -> Result<bool> {
        let target_hdr = enable && self.is_display_hdr();
        let state = self.state.get_mut().expect("d3d12 state mutex poisoned");
        if target_hdr == state.hdr_output_active {
            return Ok(state.hdr_output_active);
        }

        let target_format = if target_hdr {
            DXGI_FORMAT_R10G10B10A2_UNORM
        } else {
            DXGI_FORMAT_B8G8R8A8_UNORM
        };
        let target_colorspace = if target_hdr {
            DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
        } else {
            DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709
        };

        close_open_cmd_list_if_needed(&self.cmd_list, state);
        flush_gpu(&self.command_queue, &self.fence, self.fence_event, state)?;
        for bb in &mut state.backbuffers {
            *bb = None;
        }

        let flags = DXGI_SWAP_CHAIN_FLAG(self.swapchain_flags as i32);
        let switched = (|| -> Result<()> {
            unsafe {
                self.swapchain
                    .ResizeBuffers(
                        FRAME_COUNT as u32,
                        state.width,
                        state.height,
                        target_format,
                        flags,
                    )
                    .context("D3D12 ResizeBuffers for HDR output switch failed")?;
                self.swapchain
                    .SetColorSpace1(target_colorspace)
                    .context("D3D12 SetColorSpace1 failed")?;
            }
            Ok(())
        })();

        if let Err(err) = switched {
            if target_hdr {
                tracing::warn!(
                    "Enabling D3D12 HDR10 swapchain failed, falling back to SDR: {err:#}"
                );
                unsafe {
                    self.swapchain.ResizeBuffers(
                        FRAME_COUNT as u32,
                        state.width,
                        state.height,
                        DXGI_FORMAT_B8G8R8A8_UNORM,
                        flags,
                    )?;
                    let _ = self
                        .swapchain
                        .SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                }
                state.swapchain_format = DXGI_FORMAT_B8G8R8A8_UNORM;
                state.hdr_output_active = false;
                state.backbuffers = create_backbuffers(
                    &self.device,
                    &self.swapchain,
                    &self.rtv_heap,
                    self.rtv_inc,
                )?;
                return Ok(false);
            }
            state.swapchain_format = DXGI_FORMAT_B8G8R8A8_UNORM;
            state.hdr_output_active = false;
            state.backbuffers =
                create_backbuffers(&self.device, &self.swapchain, &self.rtv_heap, self.rtv_inc)?;
            return Err(err);
        }

        state.swapchain_format = target_format;
        state.hdr_output_active = target_hdr;
        state.backbuffers =
            create_backbuffers(&self.device, &self.swapchain, &self.rtv_heap, self.rtv_inc)?;
        Ok(state.hdr_output_active)
    }

    pub fn clear_video_cache(&mut self) {
        let state = self.state.get_mut().expect("d3d12 state mutex poisoned");
        close_open_cmd_list_if_needed(&self.cmd_list, state);
        let _ = flush_gpu(&self.command_queue, &self.fence, self.fence_event, state);
        state.sw_textures = None;
        state.fsr_src_rt = None;
        state.fsr_easu_rt = None;
    }

    pub fn begin_frame(&self, clear_rgba: [f32; 4]) {
        let mut state = self.state.lock().expect("d3d12 state mutex poisoned");
        close_open_cmd_list_if_needed(&self.cmd_list, &mut state);

        let slot_idx = unsafe { self.swapchain.GetCurrentBackBufferIndex() } as usize % FRAME_COUNT;
        if let Err(err) = prepare_frame_slot(
            &self.fence,
            self.fence_event,
            &self.cmd_list,
            &self.srv_heap,
            &self.sampler_heap,
            &mut state,
            slot_idx,
        ) {
            tracing::warn!("D3D12 begin_frame failed: {err:#}");
            return;
        }

        let Some(bb) = state.backbuffers[slot_idx].as_ref() else {
            return;
        };
        transition_subresources(
            &self.cmd_list,
            &[(
                bb,
                D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            )],
        );
        let rtv = cpu_handle_at(&self.rtv_heap, self.rtv_inc, slot_idx as u32);
        bind_rt_target(&self.cmd_list, rtv, state.width, state.height);
        unsafe {
            self.cmd_list.ClearRenderTargetView(rtv, &clear_rgba, None);
        }
    }

    pub fn present(&self, vsync: bool) -> Result<()> {
        let mut state = self.state.lock().expect("d3d12 state mutex poisoned");
        if state.frame_open {
            let slot_idx = state.active_slot;
            if let Some(bb) = state.backbuffers[slot_idx].as_ref() {
                transition_subresources(
                    &self.cmd_list,
                    &[(
                        bb,
                        D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        D3D12_RESOURCE_STATE_RENDER_TARGET,
                        D3D12_RESOURCE_STATE_PRESENT,
                    )],
                );
            }
            unsafe {
                self.cmd_list
                    .Close()
                    .context("D3D12 cmd_list.Close failed")?;
            }
            state.frame_open = false;

            for (decode_fence, fence_val) in state.pending_decode_waits.drain(..) {
                unsafe {
                    self.command_queue
                        .Wait(&decode_fence, fence_val)
                        .context("D3D12 queue.Wait(D3D12VA decode fence) failed")?;
                }
            }

            let cmd: ID3D12CommandList = self.cmd_list.cast()?;
            unsafe {
                self.command_queue.ExecuteCommandLists(&[Some(cmd)]);
            }

            let (interval, flags) = if vsync {
                (1, DXGI_PRESENT(0))
            } else if self.allow_tearing {
                (0, DXGI_PRESENT_ALLOW_TEARING)
            } else {
                (0, DXGI_PRESENT(0))
            };
            unsafe {
                self.swapchain
                    .Present(interval, flags)
                    .ok()
                    .context("D3D12 SwapChain::Present failed")?;
            }

            state.next_fence_value += 1;
            let signal_val = state.next_fence_value;
            unsafe {
                self.command_queue
                    .Signal(&self.fence, signal_val)
                    .context("D3D12 queue.Signal failed")?;
            }
            state.frames[slot_idx].fence_value = signal_val;
        }
        Ok(())
    }

    pub fn draw_video_frame(
        &mut self,
        frame: &DecodedFrame,
        options: VideoRenderOptions,
    ) -> Result<()> {
        let state = self.state.get_mut().expect("d3d12 state mutex poisoned");
        if !state.frame_open {
            return Ok(());
        }
        let (fw, fh, tw, th, d3d_format, color_mode) =
            inspect_frame(frame, state.hdr_output_active);
        let sharp = match options.filter_mode {
            VideoFilterMode::Bilinear => 0.0,
            VideoFilterMode::Cas | VideoFilterMode::Fsr1 => options.sharpness.clamp(0.0, 1.0),
        };

        if options.filter_mode == VideoFilterMode::Fsr1 {
            let upscale =
                should_run_fsr_easu(fw, fh, state.width, state.height, options.scale_mode);
            if upscale || sharp > 0.001 {
                if let Err(err) = self.draw_fsr1_frame(
                    frame,
                    fw,
                    fh,
                    tw,
                    th,
                    d3d_format,
                    color_mode,
                    sharp,
                    options.scale_mode,
                    upscale,
                ) {
                    tracing::warn!(
                        "D3D12 FSR 1.0 pass failed, falling back to single-pass draw: {err:#}"
                    );
                } else {
                    return Ok(());
                }
            }
        }

        self.draw_source_to_target(
            frame,
            fw,
            fh,
            tw,
            th,
            d3d_format,
            color_mode,
            sharp,
            options.scale_mode,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_fsr1_frame(
        &mut self,
        frame: &DecodedFrame,
        fw: u32,
        fh: u32,
        tw: u32,
        th: u32,
        d3d_format: Option<DXGI_FORMAT>,
        color_mode: f32,
        sharp: f32,
        scale_mode: ScaleMode,
        upscale: bool,
    ) -> Result<()> {
        let (vw, vh, hdr_out) = {
            let s = self.state.get_mut().expect("d3d12 state mutex poisoned");
            (s.width, s.height, s.hdr_output_active)
        };
        let pos_scale = aspect_scale(fw, fh, vw, vh, scale_mode);
        let rcas_bounds = compute_rcas_bounds(pos_scale, vw, vh);
        let fsr_params = FsrParams {
            src_size: [
                fw.max(1) as f32,
                fh.max(1) as f32,
                1.0 / (fw.max(1) as f32),
                1.0 / (fh.max(1) as f32),
            ],
            rcas_bounds,
            rcas_params: [sharp, 0.0, 0.0, 0.0],
        };

        if !upscale {
            let easu_meta = {
                let s = self.state.get_mut().expect("d3d12 state mutex poisoned");
                ensure_render_target(
                    &self.device,
                    &self.rtv_heap,
                    self.rtv_inc,
                    &mut s.fsr_easu_rt,
                    RTV_SLOT_FSR_EASU,
                    vw,
                    vh,
                    &mut s.frames[s.active_slot].retired_resources,
                )?
            };
            {
                let s = self.state.get_mut().expect("d3d12 state mutex poisoned");
                let rt = s.fsr_easu_rt.as_mut().expect("fsr_easu_rt created");
                transition_tracked(
                    &self.cmd_list,
                    &rt.resource,
                    D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    &mut rt.state,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                );
                let rtv = cpu_handle_at(&self.rtv_heap, self.rtv_inc, easu_meta.rtv_slot);
                unsafe {
                    self.cmd_list
                        .ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 1.0], None);
                }
            }

            self.draw_source_to_target(
                frame,
                fw,
                fh,
                tw,
                th,
                d3d_format,
                color_mode,
                0.0,
                scale_mode,
                Some(easu_meta),
            )?;

            let s = self.state.get_mut().expect("d3d12 state mutex poisoned");
            let rt = s.fsr_easu_rt.as_mut().expect("fsr_easu_rt created");
            transition_tracked(
                &self.cmd_list,
                &rt.resource,
                D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                &mut rt.state,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            );
            let src_res = rt.resource.clone();
            let bb_rtv = cpu_handle_at(&self.rtv_heap, self.rtv_inc, s.active_slot as u32);
            bind_rt_target(&self.cmd_list, bb_rtv, vw, vh);
            let pso = if hdr_out {
                &self.pso_r10.ps_fsr_rcas
            } else {
                &self.pso_sdr.ps_fsr_rcas
            };
            draw_fsr_quad_pass(
                &self.device,
                &self.cmd_list,
                &self.video_root_sig,
                pso,
                &self.srv_heap,
                self.srv_inc,
                s,
                &src_res,
                [1.0, 1.0, 0.0, 0.0],
                &fsr_params,
            )?;
            return Ok(());
        }

        let src_meta = {
            let s = self.state.get_mut().expect("d3d12 state mutex poisoned");
            ensure_render_target(
                &self.device,
                &self.rtv_heap,
                self.rtv_inc,
                &mut s.fsr_src_rt,
                RTV_SLOT_FSR_SRC,
                fw,
                fh,
                &mut s.frames[s.active_slot].retired_resources,
            )?
        };
        {
            let s = self.state.get_mut().expect("d3d12 state mutex poisoned");
            let rt = s.fsr_src_rt.as_mut().expect("fsr_src_rt created");
            transition_tracked(
                &self.cmd_list,
                &rt.resource,
                D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                &mut rt.state,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            );
        }

        self.draw_source_to_target(
            frame,
            fw,
            fh,
            tw,
            th,
            d3d_format,
            color_mode,
            0.0,
            ScaleMode::Stretch,
            Some(src_meta),
        )?;

        let src_res = {
            let s = self.state.get_mut().expect("d3d12 state mutex poisoned");
            let rt = s.fsr_src_rt.as_mut().expect("fsr_src_rt created");
            transition_tracked(
                &self.cmd_list,
                &rt.resource,
                D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                &mut rt.state,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            );
            rt.resource.clone()
        };

        if sharp > 0.001 {
            let easu_meta = {
                let s = self.state.get_mut().expect("d3d12 state mutex poisoned");
                ensure_render_target(
                    &self.device,
                    &self.rtv_heap,
                    self.rtv_inc,
                    &mut s.fsr_easu_rt,
                    RTV_SLOT_FSR_EASU,
                    vw,
                    vh,
                    &mut s.frames[s.active_slot].retired_resources,
                )?
            };
            {
                let s = self.state.get_mut().expect("d3d12 state mutex poisoned");
                let rt = s.fsr_easu_rt.as_mut().expect("fsr_easu_rt created");
                transition_tracked(
                    &self.cmd_list,
                    &rt.resource,
                    D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    &mut rt.state,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                );
                let rtv = cpu_handle_at(&self.rtv_heap, self.rtv_inc, easu_meta.rtv_slot);
                bind_rt_target(&self.cmd_list, rtv, vw, vh);
                unsafe {
                    self.cmd_list
                        .ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 1.0], None);
                }
                draw_fsr_quad_pass(
                    &self.device,
                    &self.cmd_list,
                    &self.video_root_sig,
                    &self.pso_r10.ps_fsr_easu,
                    &self.srv_heap,
                    self.srv_inc,
                    s,
                    &src_res,
                    pos_scale,
                    &fsr_params,
                )?;
            }

            let s = self.state.get_mut().expect("d3d12 state mutex poisoned");
            let rt = s.fsr_easu_rt.as_mut().expect("fsr_easu_rt created");
            transition_tracked(
                &self.cmd_list,
                &rt.resource,
                D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                &mut rt.state,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            );
            let easu_res = rt.resource.clone();
            let bb_rtv = cpu_handle_at(&self.rtv_heap, self.rtv_inc, s.active_slot as u32);
            bind_rt_target(&self.cmd_list, bb_rtv, vw, vh);
            let pso = if hdr_out {
                &self.pso_r10.ps_fsr_rcas
            } else {
                &self.pso_sdr.ps_fsr_rcas
            };
            draw_fsr_quad_pass(
                &self.device,
                &self.cmd_list,
                &self.video_root_sig,
                pso,
                &self.srv_heap,
                self.srv_inc,
                s,
                &easu_res,
                [1.0, 1.0, 0.0, 0.0],
                &fsr_params,
            )?;
        } else {
            let s = self.state.get_mut().expect("d3d12 state mutex poisoned");
            let bb_rtv = cpu_handle_at(&self.rtv_heap, self.rtv_inc, s.active_slot as u32);
            bind_rt_target(&self.cmd_list, bb_rtv, vw, vh);
            let pso = if hdr_out {
                &self.pso_r10.ps_fsr_easu
            } else {
                &self.pso_sdr.ps_fsr_easu
            };
            draw_fsr_quad_pass(
                &self.device,
                &self.cmd_list,
                &self.video_root_sig,
                pso,
                &self.srv_heap,
                self.srv_inc,
                s,
                &src_res,
                pos_scale,
                &fsr_params,
            )?;
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_source_to_target(
        &mut self,
        frame: &DecodedFrame,
        fw: u32,
        fh: u32,
        tw: u32,
        th: u32,
        d3d_format: Option<DXGI_FORMAT>,
        color_mode: f32,
        sharpness: f32,
        scale_mode: ScaleMode,
        target: Option<RenderTargetTex>,
    ) -> Result<()> {
        let state = self.state.get_mut().expect("d3d12 state mutex poisoned");
        let (target_w, target_h, use_r10_pso) = match target {
            Some(rt) => {
                let rtv = cpu_handle_at(&self.rtv_heap, self.rtv_inc, rt.rtv_slot);
                bind_rt_target(&self.cmd_list, rtv, rt.width, rt.height);
                (rt.width, rt.height, true)
            }
            None => {
                let rtv = cpu_handle_at(&self.rtv_heap, self.rtv_inc, state.active_slot as u32);
                bind_rt_target(&self.cmd_list, rtv, state.width, state.height);
                (state.width, state.height, state.hdr_output_active)
            }
        };

        let pso_set = if use_r10_pso {
            &self.pso_r10
        } else {
            &self.pso_sdr
        };
        let params = compute_video_params(
            fw,
            fh,
            tw,
            th,
            target_w,
            target_h,
            sharpness.clamp(0.0, 1.0),
            color_mode,
            scale_mode,
        );
        let cb_gpu =
            upload_constant_struct(&self.device, &mut state.frames[state.active_slot], &params)?;

        unsafe {
            self.cmd_list.SetGraphicsRootSignature(&self.video_root_sig);
            self.cmd_list.IASetVertexBuffers(0, None);
            self.cmd_list.IASetIndexBuffer(None);
            self.cmd_list
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            self.cmd_list.SetGraphicsRootConstantBufferView(0, cb_gpu);
            self.cmd_list.SetGraphicsRootConstantBufferView(1, cb_gpu);
        }

        match frame {
            DecodedFrame::D3D12(d3d_frame) => {
                let fmt = d3d_format.expect("D3D12 format populated");
                draw_d3d12va_frame(
                    &self.device,
                    &self.cmd_list,
                    pso_set,
                    &self.srv_heap,
                    self.srv_inc,
                    state,
                    d3d_frame,
                    fmt,
                )
            }
            DecodedFrame::D3D11(_) => {
                bail!("D3D12Renderer cannot consume zero-copy D3D11 frames directly")
            }
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
            } => draw_sw_yuv420(
                &self.device,
                &self.cmd_list,
                &pso_set.ps_yuv420,
                &self.srv_heap,
                self.srv_inc,
                state,
                y,
                u,
                v,
                *y_stride,
                *u_stride,
                *v_stride,
                *width,
                *height,
            ),
            DecodedFrame::SoftwareNv12 {
                y,
                uv,
                y_stride,
                uv_stride,
                width,
                height,
                ..
            } => {
                let (u, v, uv_s) = unpack_nv12_chroma(uv, *uv_stride, *width, *height);
                draw_sw_yuv420(
                    &self.device,
                    &self.cmd_list,
                    &pso_set.ps_yuv420,
                    &self.srv_heap,
                    self.srv_inc,
                    state,
                    y,
                    &u,
                    &v,
                    *y_stride,
                    uv_s,
                    uv_s,
                    *width,
                    *height,
                )
            }
            DecodedFrame::SoftwareP010 {
                y,
                uv,
                y_stride,
                uv_stride,
                width,
                height,
                ..
            } => {
                let (y8, u8, v8, ys, uvs) =
                    unpack_p010_to_yuv8(y, uv, *y_stride, *uv_stride, *width, *height);
                draw_sw_yuv420(
                    &self.device,
                    &self.cmd_list,
                    &pso_set.ps_yuv420,
                    &self.srv_heap,
                    self.srv_inc,
                    state,
                    &y8,
                    &u8,
                    &v8,
                    ys,
                    uvs,
                    uvs,
                    *width,
                    *height,
                )
            }
        }
    }

    pub fn upsert_ui_texture(&mut self, id: u64, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
        if w == 0 || h == 0 {
            return Ok(());
        }
        let expected = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(4))
            .context("UI texture size overflow")?;
        if rgba.len() < expected {
            bail!(
                "UI texture {id} pixel buffer too small: expected {expected}, got {}",
                rgba.len()
            );
        }

        let state = self.state.get_mut().expect("d3d12 state mutex poisoned");
        let existing_same = state
            .ui_textures
            .get(&id)
            .is_some_and(|e| e.width == w && e.height == h);
        if existing_same {
            return self.update_ui_texture_region(id, 0, 0, w, h, rgba);
        }

        if let Some(prev) = state.ui_textures.remove(&id) {
            let retired = &mut state.frames[state.active_slot];
            retired.retired_resources.push(prev.resource);
            retired.retired_ui_slots.push(prev.srv_slot);
        }
        let slot = if let Some(reused) = state.ui_free_slots.pop() {
            reused
        } else {
            if state.ui_next_slot >= UI_TEXTURE_SLOTS {
                bail!("D3D12 UI texture descriptor capacity ({UI_TEXTURE_SLOTS}) exceeded");
            }
            let s = state.ui_next_slot;
            state.ui_next_slot += 1;
            s
        };

        let resource = create_texture_2d(
            &self.device,
            w,
            h,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            D3D12_RESOURCE_FLAG_NONE,
            None,
            D3D12_RESOURCE_STATE_COMMON,
        )?;
        let srv_desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: D3D12_TEX2D_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                    PlaneSlice: 0,
                    ResourceMinLODClamp: 0.0,
                },
            },
        };
        let cpu_handle = cpu_handle_at(&self.srv_heap, self.srv_inc, slot);
        unsafe {
            self.device
                .CreateShaderResourceView(&resource, Some(&srv_desc), cpu_handle);
        }

        state.ui_textures.insert(
            id,
            UiTextureEntry {
                resource,
                state: D3D12_RESOURCE_STATE_COMMON,
                width: w,
                height: h,
                srv_slot: slot,
            },
        );
        self.update_ui_texture_region(id, 0, 0, w, h, rgba)
    }

    pub fn update_ui_texture_region(
        &mut self,
        id: u64,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        rgba: &[u8],
    ) -> Result<()> {
        if w == 0 || h == 0 {
            return Ok(());
        }
        let expected = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(4))
            .context("UI texture region size overflow")?;
        if rgba.len() < expected {
            bail!(
                "UI texture region {id} buffer too small: expected {expected}, got {}",
                rgba.len()
            );
        }

        let state = self.state.get_mut().expect("d3d12 state mutex poisoned");
        let entry = state
            .ui_textures
            .get(&id)
            .with_context(|| format!("missing D3D12 UI texture id {id}"))?;
        if x.saturating_add(w) > entry.width || y.saturating_add(h) > entry.height {
            bail!(
                "UI texture region out of bounds: ({x},{y})+{w}x{h} vs {}x{}",
                entry.width,
                entry.height
            );
        }

        let immediate = !state.frame_open;
        if immediate {
            let slot_idx = state.active_slot;
            prepare_frame_slot(
                &self.fence,
                self.fence_event,
                &self.cmd_list,
                &self.srv_heap,
                &self.sampler_heap,
                state,
                slot_idx,
            )?;
        }

        let upload_res_res = {
            let slot = &mut state.frames[state.active_slot];
            let entry = state.ui_textures.get_mut(&id).expect("checked above");
            let tex_res = entry.resource.clone();
            transition_tracked(
                &self.cmd_list,
                &tex_res,
                0,
                &mut entry.state,
                D3D12_RESOURCE_STATE_COPY_DEST,
            );
            upload_texture_region(
                &self.device,
                &self.cmd_list,
                slot,
                &tex_res,
                DXGI_FORMAT_R8G8B8A8_UNORM,
                x,
                y,
                w,
                h,
                rgba,
                (w as usize) * 4,
            )?;
            transition_tracked(
                &self.cmd_list,
                &tex_res,
                0,
                &mut entry.state,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            );
            Ok::<(), anyhow::Error>(())
        };

        if immediate {
            let _ = unsafe { self.cmd_list.Close() };
            state.frame_open = false;
            upload_res_res?;
            let cmd: ID3D12CommandList = self.cmd_list.cast()?;
            unsafe {
                self.command_queue.ExecuteCommandLists(&[Some(cmd)]);
            }
            flush_gpu(&self.command_queue, &self.fence, self.fence_event, state)?;
            return Ok(());
        }

        upload_res_res
    }

    pub fn free_ui_texture(&mut self, id: u64) {
        let state = self.state.get_mut().expect("d3d12 state mutex poisoned");
        if let Some(entry) = state.ui_textures.remove(&id) {
            let slot = &mut state.frames[state.active_slot];
            slot.retired_resources.push(entry.resource);
            slot.retired_ui_slots.push(entry.srv_slot);
        }
    }

    pub fn draw_ui_mesh(&mut self, draw: UiMeshDraw<'_>) -> Result<()> {
        let state = self.state.get_mut().expect("d3d12 state mutex poisoned");
        if !state.frame_open || draw.vertices_bytes.is_empty() || draw.indices.is_empty() {
            return Ok(());
        }
        let Some(entry) = state.ui_textures.get_mut(&draw.tex_id) else {
            tracing::warn!(
                "Skipping D3D12 UI draw referencing unknown texture id {}",
                draw.tex_id
            );
            return Ok(());
        };
        let tex_res = entry.resource.clone();
        let tex_srv_slot = entry.srv_slot;
        transition_tracked(
            &self.cmd_list,
            &tex_res,
            0,
            &mut entry.state,
            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
        );

        let slot = &mut state.frames[state.active_slot];
        let vb_gpu = upload_bytes_to_gpu(&self.device, slot, draw.vertices_bytes, 4)?;
        let ib_bytes_len = std::mem::size_of_val(draw.indices);
        let ib_slice =
            unsafe { std::slice::from_raw_parts(draw.indices.as_ptr().cast::<u8>(), ib_bytes_len) };
        let ib_gpu = upload_bytes_to_gpu(&self.device, slot, ib_slice, 4)?;

        let bb_rtv = cpu_handle_at(&self.rtv_heap, self.rtv_inc, state.active_slot as u32);
        let vp = D3D12_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: state.width.max(1) as f32,
            Height: state.height.max(1) as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let scissor = RECT {
            left: draw.scissor[0],
            top: draw.scissor[1],
            right: draw.scissor[2],
            bottom: draw.scissor[3],
        };
        let vb_view = D3D12_VERTEX_BUFFER_VIEW {
            BufferLocation: vb_gpu,
            SizeInBytes: draw.vertices_bytes.len() as u32,
            StrideInBytes: EGUI_VERTEX_STRIDE,
        };
        let ib_view = D3D12_INDEX_BUFFER_VIEW {
            BufferLocation: ib_gpu,
            SizeInBytes: ib_bytes_len as u32,
            Format: DXGI_FORMAT_R32_UINT,
        };

        let root_consts: [f32; 4] = [
            draw.screen_size_pts[0],
            draw.screen_size_pts[1],
            draw.hdr_paper_white_nits,
            0.0,
        ];
        let srv_gpu = gpu_handle_at(&self.srv_heap, self.srv_inc, tex_srv_slot);
        let sampler_idx = (if draw.linear_filter { 0 } else { 3 })
            + match draw.wrap {
                UiTextureWrap::ClampToEdge => 0,
                UiTextureWrap::Repeat => 1,
                UiTextureWrap::MirroredRepeat => 2,
            };
        let sampler_gpu = gpu_handle_at(&self.sampler_heap, self.sampler_inc, sampler_idx);
        let pso = if state.hdr_output_active {
            &self.pso_r10.ui
        } else {
            &self.pso_sdr.ui
        };

        unsafe {
            self.cmd_list
                .OMSetRenderTargets(1, Some(&bb_rtv), false, None);
            self.cmd_list.RSSetViewports(&[vp]);
            self.cmd_list.RSSetScissorRects(&[scissor]);
            self.cmd_list.SetPipelineState(pso);
            self.cmd_list.SetGraphicsRootSignature(&self.ui_root_sig);
            self.cmd_list
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.cmd_list.IASetVertexBuffers(0, Some(&[vb_view]));
            self.cmd_list.IASetIndexBuffer(Some(&ib_view));
            self.cmd_list
                .SetGraphicsRoot32BitConstants(0, 4, root_consts.as_ptr().cast(), 0);
            self.cmd_list.SetGraphicsRootDescriptorTable(1, srv_gpu);
            self.cmd_list.SetGraphicsRootDescriptorTable(2, sampler_gpu);
            self.cmd_list
                .DrawIndexedInstanced(draw.indices.len() as u32, 1, 0, 0, 0);
        }
        Ok(())
    }
}

impl VideoRenderer for D3D12Renderer {
    fn viewport_size(&self) -> (u32, u32) {
        self.viewport_size()
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.resize(width, height)
    }

    fn is_display_hdr(&self) -> bool {
        self.is_display_hdr()
    }

    fn is_hdr_output_active(&self) -> bool {
        self.is_hdr_output_active()
    }

    fn set_hdr_output(&mut self, enable: bool) -> Result<bool> {
        self.set_hdr_output(enable)
    }

    fn wait_for_frame_latency(&self, timeout_ms: u32) {
        self.wait_for_frame_latency(timeout_ms);
    }

    fn clear_video_cache(&mut self) {
        self.clear_video_cache();
    }

    fn begin_frame(&self, clear_rgba: [f32; 4]) {
        self.begin_frame(clear_rgba);
    }

    fn draw_video_frame(
        &mut self,
        frame: &DecodedFrame,
        options: VideoRenderOptions,
    ) -> Result<()> {
        self.draw_video_frame(frame, options)
    }

    fn present(&self, vsync: bool) -> Result<()> {
        self.present(vsync)
    }
}

impl UiOverlayRenderer for D3D12Renderer {
    fn upsert_ui_texture(&mut self, id: u64, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
        self.upsert_ui_texture(id, w, h, rgba)
    }

    fn update_ui_texture_region(
        &mut self,
        id: u64,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        rgba: &[u8],
    ) -> Result<()> {
        self.update_ui_texture_region(id, x, y, w, h, rgba)
    }

    fn free_ui_texture(&mut self, id: u64) {
        self.free_ui_texture(id);
    }

    fn draw_ui_mesh(&mut self, draw: UiMeshDraw<'_>) -> Result<()> {
        self.draw_ui_mesh(draw)
    }
}

impl Drop for D3D12Renderer {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut() {
            close_open_cmd_list_if_needed(&self.cmd_list, state);
            let _ = flush_gpu(&self.command_queue, &self.fence, self.fence_event, state);
            state.ui_textures.clear();
            state.sw_textures = None;
            state.fsr_src_rt = None;
            state.fsr_easu_rt = None;
            for bb in &mut state.backbuffers {
                *bb = None;
            }
        }
        if !self.fence_event.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.fence_event);
            }
        }
        if let Some(handle) = self.frame_latency_waitable.take()
            && !handle.is_invalid()
        {
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }
}

pub(crate) fn align_up(value: usize, alignment: usize) -> usize {
    debug_assert!(alignment.is_power_of_two());
    (value + alignment - 1) & !(alignment - 1)
}

pub(crate) fn calc_plane_subresource(
    slice: u32,
    plane: u32,
    mip_levels: u32,
    array_size: u32,
) -> u32 {
    slice * mip_levels + plane * mip_levels * array_size
}

fn inspect_frame(
    frame: &DecodedFrame,
    hdr_output_active: bool,
) -> (u32, u32, u32, u32, Option<DXGI_FORMAT>, f32) {
    match frame {
        DecodedFrame::D3D12(f) => {
            let desc = unsafe { f.texture.GetDesc() };
            let mode = if hdr_output_active { 1.0 } else { 0.0 };
            (
                f.width,
                f.height,
                (desc.Width as u32).max(f.width),
                desc.Height.max(f.height),
                Some(desc.Format),
                mode,
            )
        }
        DecodedFrame::SoftwareYuv420 {
            width,
            height,
            is_hdr,
            ..
        }
        | DecodedFrame::SoftwareNv12 {
            width,
            height,
            is_hdr,
            ..
        }
        | DecodedFrame::SoftwareP010 {
            width,
            height,
            is_hdr,
            ..
        } => {
            let mode = match (*is_hdr, hdr_output_active) {
                (false, false) => 0.0,
                (true, false) => 1.0,
                (true, true) => 2.0,
                (false, true) => 3.0,
            };
            (*width, *height, *width, *height, None, mode)
        }
        DecodedFrame::D3D11(f) => (f.width, f.height, f.width, f.height, None, 0.0),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_d3d12va_frame(
    device: &ID3D12Device,
    cmd_list: &ID3D12GraphicsCommandList,
    pso_set: &PipelineFormatSet,
    srv_heap: &ID3D12DescriptorHeap,
    srv_inc: u32,
    state: &mut RendererState,
    frame: &D3D12Frame,
    tex_format: DXGI_FORMAT,
) -> Result<()> {
    let is_p010 = tex_format == DXGI_FORMAT_P010;
    let desc = unsafe { frame.texture.GetDesc() };
    let array_size = u32::from(desc.DepthOrArraySize).max(1);
    let mip_levels = u32::from(desc.MipLevels).max(1);
    let slice = frame.subresource_index.min(array_size - 1);
    let plane0_sub = calc_plane_subresource(slice, 0, mip_levels, array_size);
    let plane1_sub = calc_plane_subresource(slice, 1, mip_levels, array_size);

    let (y_fmt, uv_fmt) = if is_p010 {
        (DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM)
    } else {
        (DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM)
    };

    let srv_start =
        alloc_transient_srvs(&mut state.frames[state.active_slot], state.active_slot, 3)?;
    let y_desc = array_plane_srv_desc(y_fmt, slice, 0);
    let uv_desc = array_plane_srv_desc(uv_fmt, slice, 1);
    let null_desc = null_tex2d_srv_desc();

    unsafe {
        device.CreateShaderResourceView(
            &frame.texture,
            Some(&y_desc),
            cpu_handle_at(srv_heap, srv_inc, srv_start),
        );
        device.CreateShaderResourceView(
            &frame.texture,
            Some(&uv_desc),
            cpu_handle_at(srv_heap, srv_inc, srv_start + 1),
        );
        device.CreateShaderResourceView(
            None,
            Some(&null_desc),
            cpu_handle_at(srv_heap, srv_inc, srv_start + 2),
        );
    }

    if let Some(ref decode_fence) = frame.fence {
        record_decode_wait(
            &mut state.pending_decode_waits,
            decode_fence,
            frame.fence_value,
        );
    }
    state.frames[state.active_slot]
        .pinned_frames
        .push(frame.pin_guard());

    transition_subresources(
        cmd_list,
        &[
            (
                &frame.texture,
                plane0_sub,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            ),
            (
                &frame.texture,
                plane1_sub,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            ),
        ],
    );

    let pso = if is_p010 {
        &pso_set.ps_p010
    } else {
        &pso_set.ps_nv12
    };
    let table_gpu = gpu_handle_at(srv_heap, srv_inc, srv_start);
    unsafe {
        cmd_list.SetPipelineState(pso);
        cmd_list.SetGraphicsRootDescriptorTable(2, table_gpu);
        cmd_list.DrawInstanced(4, 1, 0, 0);
    }

    transition_subresources(
        cmd_list,
        &[
            (
                &frame.texture,
                plane0_sub,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            ),
            (
                &frame.texture,
                plane1_sub,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            ),
        ],
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_sw_yuv420(
    device: &ID3D12Device,
    cmd_list: &ID3D12GraphicsCommandList,
    pso: &ID3D12PipelineState,
    srv_heap: &ID3D12DescriptorHeap,
    srv_inc: u32,
    state: &mut RendererState,
    y: &[u8],
    u: &[u8],
    v: &[u8],
    y_stride: usize,
    u_stride: usize,
    v_stride: usize,
    width: u32,
    height: u32,
) -> Result<()> {
    let recreate = state
        .sw_textures
        .as_ref()
        .is_none_or(|t| t.width != width || t.height != height);
    if recreate {
        if let Some(old) = state.sw_textures.take() {
            let retired = &mut state.frames[state.active_slot].retired_resources;
            retired.push(old.y.resource);
            retired.push(old.u.resource);
            retired.push(old.v.resource);
        }
        state.sw_textures = Some(create_sw_yuv_textures(device, width, height)?);
    }

    let cw = width.div_ceil(2);
    let ch = height.div_ceil(2);
    let active_slot = state.active_slot;
    let (y_res, u_res, v_res) = {
        let texs = state.sw_textures.as_mut().expect("sw_textures initialized");
        let slot = &mut state.frames[active_slot];
        upload_sw_plane(
            device,
            cmd_list,
            slot,
            &mut texs.y,
            y,
            y_stride,
            width,
            height,
        )?;
        upload_sw_plane(device, cmd_list, slot, &mut texs.u, u, u_stride, cw, ch)?;
        upload_sw_plane(device, cmd_list, slot, &mut texs.v, v, v_stride, cw, ch)?;
        (
            texs.y.resource.clone(),
            texs.u.resource.clone(),
            texs.v.resource.clone(),
        )
    };

    let srv_start = alloc_transient_srvs(&mut state.frames[active_slot], active_slot, 3)?;
    let r8_desc = tex2d_srv_desc(DXGI_FORMAT_R8_UNORM);
    unsafe {
        device.CreateShaderResourceView(
            &y_res,
            Some(&r8_desc),
            cpu_handle_at(srv_heap, srv_inc, srv_start),
        );
        device.CreateShaderResourceView(
            &u_res,
            Some(&r8_desc),
            cpu_handle_at(srv_heap, srv_inc, srv_start + 1),
        );
        device.CreateShaderResourceView(
            &v_res,
            Some(&r8_desc),
            cpu_handle_at(srv_heap, srv_inc, srv_start + 2),
        );
        cmd_list.SetPipelineState(pso);
        cmd_list.SetGraphicsRootDescriptorTable(2, gpu_handle_at(srv_heap, srv_inc, srv_start));
        cmd_list.DrawInstanced(4, 1, 0, 0);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_fsr_quad_pass(
    device: &ID3D12Device,
    cmd_list: &ID3D12GraphicsCommandList,
    root_sig: &ID3D12RootSignature,
    pso: &ID3D12PipelineState,
    srv_heap: &ID3D12DescriptorHeap,
    srv_inc: u32,
    state: &mut RendererState,
    src_tex: &ID3D12Resource,
    pos_scale: [f32; 4],
    fsr_params: &FsrParams,
) -> Result<()> {
    let vs_params = VideoParams {
        pos_scale,
        ..VideoParams::default()
    };
    let active_slot = state.active_slot;
    let vs_cb_gpu = upload_constant_struct(device, &mut state.frames[active_slot], &vs_params)?;
    let ps_cb_gpu = upload_constant_struct(device, &mut state.frames[active_slot], fsr_params)?;
    let srv_start = alloc_transient_srvs(&mut state.frames[active_slot], active_slot, 3)?;
    let src_desc = tex2d_srv_desc(DXGI_FORMAT_R10G10B10A2_UNORM);
    let null_desc = null_tex2d_srv_desc();

    unsafe {
        device.CreateShaderResourceView(
            src_tex,
            Some(&src_desc),
            cpu_handle_at(srv_heap, srv_inc, srv_start),
        );
        device.CreateShaderResourceView(
            None,
            Some(&null_desc),
            cpu_handle_at(srv_heap, srv_inc, srv_start + 1),
        );
        device.CreateShaderResourceView(
            None,
            Some(&null_desc),
            cpu_handle_at(srv_heap, srv_inc, srv_start + 2),
        );
        cmd_list.SetPipelineState(pso);
        cmd_list.SetGraphicsRootSignature(root_sig);
        cmd_list.IASetVertexBuffers(0, None);
        cmd_list.IASetIndexBuffer(None);
        cmd_list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        cmd_list.SetGraphicsRootConstantBufferView(0, vs_cb_gpu);
        cmd_list.SetGraphicsRootConstantBufferView(1, ps_cb_gpu);
        cmd_list.SetGraphicsRootDescriptorTable(2, gpu_handle_at(srv_heap, srv_inc, srv_start));
        cmd_list.DrawInstanced(4, 1, 0, 0);
    }
    Ok(())
}

fn create_d3d12_device() -> Result<(IDXGIFactory4, ID3D12Device)> {
    let factory: IDXGIFactory4 =
        unsafe { CreateDXGIFactory1().context("CreateDXGIFactory1 failed")? };

    let mut i = 0;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(i) } {
        i += 1;
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if (DXGI_ADAPTER_FLAG(desc.Flags as i32) & DXGI_ADAPTER_FLAG_SOFTWARE)
            != DXGI_ADAPTER_FLAG(0)
        {
            continue;
        }
        let mut dev: Option<ID3D12Device> = None;
        if unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut dev) }.is_ok()
            && let Some(device) = dev
        {
            return Ok((factory, device));
        }
    }

    let mut dev: Option<ID3D12Device> = None;
    unsafe {
        D3D12CreateDevice(None::<&IUnknown>, D3D_FEATURE_LEVEL_11_0, &mut dev)
            .context("D3D12CreateDevice failed on default adapter")?;
    }
    Ok((factory, dev.context("D3D12CreateDevice returned None")?))
}

fn create_backbuffers(
    device: &ID3D12Device,
    swapchain: &IDXGISwapChain3,
    rtv_heap: &ID3D12DescriptorHeap,
    rtv_inc: u32,
) -> Result<[Option<ID3D12Resource>; FRAME_COUNT]> {
    let mut out: [Option<ID3D12Resource>; FRAME_COUNT] = [None, None];
    for (idx, slot) in out.iter_mut().enumerate() {
        let bb: ID3D12Resource = unsafe {
            swapchain
                .GetBuffer(idx as u32)
                .with_context(|| format!("D3D12 GetBuffer({idx}) failed"))?
        };
        let handle = cpu_handle_at(rtv_heap, rtv_inc, idx as u32);
        unsafe {
            device.CreateRenderTargetView(&bb, None, handle);
        }
        *slot = Some(bb);
    }
    Ok(out)
}

fn create_frame_slot(device: &ID3D12Device) -> Result<FrameSlot> {
    let allocator: ID3D12CommandAllocator = unsafe {
        device
            .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
            .context("CreateCommandAllocator(DIRECT) failed")?
    };
    let upload = UploadArena::new(device, UPLOAD_ARENA_BYTES)?;
    Ok(FrameSlot {
        allocator,
        upload,
        transient_srv_cursor: 0,
        fence_value: 0,
        pinned_frames: Vec::new(),
        retired_resources: Vec::new(),
        retired_ui_slots: Vec::new(),
        overflow_uploads: Vec::new(),
    })
}

fn prepare_frame_slot(
    fence: &ID3D12Fence,
    fence_event: HANDLE,
    cmd_list: &ID3D12GraphicsCommandList,
    srv_heap: &ID3D12DescriptorHeap,
    sampler_heap: &ID3D12DescriptorHeap,
    state: &mut RendererState,
    slot_idx: usize,
) -> Result<()> {
    let wait_val = state.frames[slot_idx].fence_value;
    wait_for_fence_value(fence, fence_event, wait_val)?;
    {
        let slot = &mut state.frames[slot_idx];
        slot.pinned_frames.clear();
        slot.retired_resources.clear();
        slot.overflow_uploads.clear();
        slot.transient_srv_cursor = 0;
        slot.upload.cursor = 0;
        state.ui_free_slots.append(&mut slot.retired_ui_slots);
    }
    let allocator = state.frames[slot_idx].allocator.clone();
    unsafe {
        allocator
            .Reset()
            .context("D3D12 CommandAllocator::Reset failed")?;
        cmd_list
            .Reset(&allocator, None)
            .context("D3D12 GraphicsCommandList::Reset failed")?;
        cmd_list.SetDescriptorHeaps(&[Some(srv_heap.clone()), Some(sampler_heap.clone())]);
    }
    state.active_slot = slot_idx;
    state.frame_open = true;
    state.pending_decode_waits.clear();
    Ok(())
}

fn close_open_cmd_list_if_needed(cmd_list: &ID3D12GraphicsCommandList, state: &mut RendererState) {
    if state.frame_open {
        unsafe {
            let _ = cmd_list.Close();
        }
        state.frame_open = false;
        state.pending_decode_waits.clear();
    }
}

fn flush_gpu(
    queue: &ID3D12CommandQueue,
    fence: &ID3D12Fence,
    fence_event: HANDLE,
    state: &mut RendererState,
) -> Result<()> {
    state.next_fence_value += 1;
    let target = state.next_fence_value;
    unsafe {
        queue
            .Signal(fence, target)
            .context("D3D12 flush Signal failed")?;
    }
    wait_for_fence_value(fence, fence_event, target)?;
    for slot in &mut state.frames {
        slot.fence_value = target;
        slot.pinned_frames.clear();
        slot.retired_resources.clear();
        slot.overflow_uploads.clear();
        slot.upload.cursor = 0;
        slot.transient_srv_cursor = 0;
        state.ui_free_slots.append(&mut slot.retired_ui_slots);
    }
    Ok(())
}

fn wait_for_fence_value(fence: &ID3D12Fence, event: HANDLE, target: u64) -> Result<()> {
    if target == 0 {
        return Ok(());
    }
    unsafe {
        if fence.GetCompletedValue() < target {
            fence
                .SetEventOnCompletion(target, event)
                .context("ID3D12Fence::SetEventOnCompletion failed")?;
            let _ = WaitForSingleObjectEx(event, INFINITE, false);
        }
    }
    Ok(())
}

fn bind_rt_target(
    cmd_list: &ID3D12GraphicsCommandList,
    rtv: D3D12_CPU_DESCRIPTOR_HANDLE,
    width: u32,
    height: u32,
) {
    let vp = D3D12_VIEWPORT {
        TopLeftX: 0.0,
        TopLeftY: 0.0,
        Width: width.max(1) as f32,
        Height: height.max(1) as f32,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    };
    let scissor = RECT {
        left: 0,
        top: 0,
        right: width.max(1) as i32,
        bottom: height.max(1) as i32,
    };
    unsafe {
        cmd_list.OMSetRenderTargets(1, Some(&rtv), false, None);
        cmd_list.RSSetViewports(&[vp]);
        cmd_list.RSSetScissorRects(&[scissor]);
    }
}

fn transition_tracked(
    cmd_list: &ID3D12GraphicsCommandList,
    res: &ID3D12Resource,
    subresource: u32,
    state_ref: &mut D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) {
    if *state_ref == after {
        return;
    }
    transition_subresources(cmd_list, &[(res, subresource, *state_ref, after)]);
    *state_ref = after;
}

fn transition_subresources(
    cmd_list: &ID3D12GraphicsCommandList,
    transitions: &[(
        &ID3D12Resource,
        u32,
        D3D12_RESOURCE_STATES,
        D3D12_RESOURCE_STATES,
    )],
) {
    let mut barriers: Vec<D3D12_RESOURCE_BARRIER> = transitions
        .iter()
        .filter(|(_, _, before, after)| before != after)
        .map(|(res, subresource, before, after)| D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: ManuallyDrop::new(Some((*res).clone())),
                    Subresource: *subresource,
                    StateBefore: *before,
                    StateAfter: *after,
                }),
            },
        })
        .collect();
    if barriers.is_empty() {
        return;
    }
    // SAFETY: Each `ManuallyDrop` resource clone is valid across `ResourceBarrier` and is
    // explicitly dropped immediately afterward so zero COM references leak.
    unsafe {
        cmd_list.ResourceBarrier(&barriers);
        for b in &mut barriers {
            ManuallyDrop::drop(&mut (*b.Anonymous.Transition).pResource);
        }
    }
}

fn upload_constant_struct<T: Copy>(
    device: &ID3D12Device,
    slot: &mut FrameSlot,
    val: &T,
) -> Result<u64> {
    let raw_bytes = unsafe {
        std::slice::from_raw_parts((val as *const T).cast::<u8>(), std::mem::size_of::<T>())
    };
    upload_bytes_to_gpu(
        device,
        slot,
        raw_bytes,
        D3D12_CONSTANT_BUFFER_DATA_PLACEMENT_ALIGNMENT as usize,
    )
}

fn upload_bytes_to_gpu(
    device: &ID3D12Device,
    slot: &mut FrameSlot,
    data: &[u8],
    alignment: usize,
) -> Result<u64> {
    if data.is_empty() {
        return Ok(slot.upload.gpu_addr);
    }
    let aligned_size = align_up(data.len(), alignment);
    if let Some((gpu_addr, dst_ptr, _)) = slot.upload.alloc(aligned_size, alignment) {
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), dst_ptr, data.len());
        }
        return Ok(gpu_addr);
    }

    let overflow = create_upload_buffer(device, aligned_size)?;
    let mut ptr = std::ptr::null_mut();
    unsafe {
        overflow.Map(0, None, Some(&mut ptr))?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.cast::<u8>(), data.len());
        overflow.Unmap(0, None);
    }
    let gpu_addr = unsafe { overflow.GetGPUVirtualAddress() };
    slot.overflow_uploads.push(overflow);
    Ok(gpu_addr)
}

#[allow(clippy::too_many_arguments)]
fn upload_sw_plane(
    device: &ID3D12Device,
    cmd_list: &ID3D12GraphicsCommandList,
    slot: &mut FrameSlot,
    plane: &mut SwPlane,
    data: &[u8],
    src_stride: usize,
    width: u32,
    height: u32,
) -> Result<()> {
    let res = plane.resource.clone();
    transition_tracked(
        cmd_list,
        &res,
        0,
        &mut plane.state,
        D3D12_RESOURCE_STATE_COPY_DEST,
    );
    upload_texture_region(
        device,
        cmd_list,
        slot,
        &res,
        DXGI_FORMAT_R8_UNORM,
        0,
        0,
        width,
        height,
        data,
        src_stride,
    )?;
    transition_tracked(
        cmd_list,
        &res,
        0,
        &mut plane.state,
        D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn upload_texture_region(
    device: &ID3D12Device,
    cmd_list: &ID3D12GraphicsCommandList,
    slot: &mut FrameSlot,
    dst_tex: &ID3D12Resource,
    format: DXGI_FORMAT,
    dst_x: u32,
    dst_y: u32,
    width: u32,
    height: u32,
    data: &[u8],
    src_stride: usize,
) -> Result<()> {
    let bpp = match format {
        DXGI_FORMAT_R8G8B8A8_UNORM => 4usize,
        _ => 1usize,
    };
    let copy_row_bytes = (width as usize) * bpp;
    let dst_row_pitch = align_up(copy_row_bytes, D3D12_TEXTURE_DATA_PITCH_ALIGNMENT as usize);
    let total_bytes = dst_row_pitch * (height as usize);
    let placement = D3D12_TEXTURE_DATA_PLACEMENT_ALIGNMENT as usize;

    let (upload_res, offset) =
        if let Some((_, dst_ptr, arena_offset)) = slot.upload.alloc(total_bytes, placement) {
            copy_strided_rows(
                data,
                src_stride,
                dst_ptr,
                dst_row_pitch,
                copy_row_bytes,
                height,
            );
            (slot.upload.resource.clone(), arena_offset as u64)
        } else {
            let overflow = create_upload_buffer(device, total_bytes)?;
            let mut ptr = std::ptr::null_mut();
            unsafe {
                overflow.Map(0, None, Some(&mut ptr))?;
                copy_strided_rows(
                    data,
                    src_stride,
                    ptr.cast::<u8>(),
                    dst_row_pitch,
                    copy_row_bytes,
                    height,
                );
                overflow.Unmap(0, None);
            }
            slot.overflow_uploads.push(overflow.clone());
            (overflow, 0)
        };

    let mut dst_loc = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(dst_tex.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: 0,
        },
    };
    let mut src_loc = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(upload_res)),
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                Offset: offset,
                Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                    Format: format,
                    Width: width,
                    Height: height,
                    Depth: 1,
                    RowPitch: dst_row_pitch as u32,
                },
            },
        },
    };
    let src_box = D3D12_BOX {
        left: 0,
        top: 0,
        front: 0,
        right: width,
        bottom: height,
        back: 1,
    };
    unsafe {
        cmd_list.CopyTextureRegion(&dst_loc, dst_x, dst_y, 0, &src_loc, Some(&src_box));
        ManuallyDrop::drop(&mut dst_loc.pResource);
        ManuallyDrop::drop(&mut src_loc.pResource);
    }
    Ok(())
}

fn copy_strided_rows(
    src: &[u8],
    src_stride: usize,
    dst_ptr: *mut u8,
    dst_stride: usize,
    copy_row_bytes: usize,
    height: u32,
) {
    for row in 0..height as usize {
        let s0 = row * src_stride;
        let s1 = s0 + copy_row_bytes;
        if s1 <= src.len() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    src[s0..s1].as_ptr(),
                    dst_ptr.add(row * dst_stride),
                    copy_row_bytes,
                );
            }
        }
    }
}

fn alloc_transient_srvs(slot: &mut FrameSlot, frame_idx: usize, count: u32) -> Result<u32> {
    let end = slot.transient_srv_cursor + count;
    if end > TRANSIENT_SRV_SLOTS_PER_FRAME {
        bail!("D3D12 per-frame transient SRV capacity ({TRANSIENT_SRV_SLOTS_PER_FRAME}) exceeded");
    }
    let base = UI_TEXTURE_SLOTS + (frame_idx as u32) * TRANSIENT_SRV_SLOTS_PER_FRAME;
    let start = base + slot.transient_srv_cursor;
    slot.transient_srv_cursor = end;
    Ok(start)
}

fn record_decode_wait(waits: &mut Vec<(ID3D12Fence, u64)>, fence: &ID3D12Fence, value: u64) {
    let raw = fence.as_raw();
    if let Some((_, max_val)) = waits.iter_mut().find(|(f, _)| f.as_raw() == raw) {
        *max_val = (*max_val).max(value);
    } else {
        waits.push((fence.clone(), value));
    }
}

#[allow(clippy::too_many_arguments)]
fn ensure_render_target(
    device: &ID3D12Device,
    rtv_heap: &ID3D12DescriptorHeap,
    rtv_inc: u32,
    slot: &mut Option<OwnedRenderTarget>,
    rtv_slot: u32,
    width: u32,
    height: u32,
    retired: &mut Vec<ID3D12Resource>,
) -> Result<RenderTargetTex> {
    let w = width.max(1);
    let h = height.max(1);
    if let Some(cur) = slot.as_ref()
        && cur.meta.width == w
        && cur.meta.height == h
    {
        return Ok(cur.meta);
    }
    if let Some(old) = slot.take() {
        retired.push(old.resource);
    }

    let clear = D3D12_CLEAR_VALUE {
        Format: DXGI_FORMAT_R10G10B10A2_UNORM,
        Anonymous: D3D12_CLEAR_VALUE_0 {
            Color: [0.0, 0.0, 0.0, 1.0],
        },
    };
    let resource = create_texture_2d(
        device,
        w,
        h,
        DXGI_FORMAT_R10G10B10A2_UNORM,
        D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
        Some(&clear),
        D3D12_RESOURCE_STATE_RENDER_TARGET,
    )?;
    let rtv_handle = cpu_handle_at(rtv_heap, rtv_inc, rtv_slot);
    unsafe {
        device.CreateRenderTargetView(&resource, None, rtv_handle);
    }
    let meta = RenderTargetTex {
        width: w,
        height: h,
        rtv_slot,
    };
    *slot = Some(OwnedRenderTarget {
        meta,
        resource,
        state: D3D12_RESOURCE_STATE_RENDER_TARGET,
    });
    Ok(meta)
}

fn create_sw_yuv_textures(device: &ID3D12Device, width: u32, height: u32) -> Result<SwYuvTextures> {
    let cw = width.div_ceil(2);
    let ch = height.div_ceil(2);
    let make_plane = |pw: u32, ph: u32| -> Result<SwPlane> {
        let resource = create_texture_2d(
            device,
            pw.max(1),
            ph.max(1),
            DXGI_FORMAT_R8_UNORM,
            D3D12_RESOURCE_FLAG_NONE,
            None,
            D3D12_RESOURCE_STATE_COMMON,
        )?;
        Ok(SwPlane {
            resource,
            state: D3D12_RESOURCE_STATE_COMMON,
        })
    };
    Ok(SwYuvTextures {
        width,
        height,
        y: make_plane(width, height)?,
        u: make_plane(cw, ch)?,
        v: make_plane(cw, ch)?,
    })
}

fn create_upload_buffer(device: &ID3D12Device, bytes: usize) -> Result<ID3D12Resource> {
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_UPLOAD,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
        MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
        CreationNodeMask: 0,
        VisibleNodeMask: 0,
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        Width: bytes.max(256) as u64,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_NONE,
    };
    let mut res = None;
    unsafe {
        device
            .CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                &mut res,
            )
            .context("CreateCommittedResource(UPLOAD buffer) failed")?;
    }
    res.context("CreateCommittedResource(UPLOAD) returned None")
}

fn create_texture_2d(
    device: &ID3D12Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    flags: windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_FLAGS,
    clear_value: Option<&D3D12_CLEAR_VALUE>,
    initial_state: D3D12_RESOURCE_STATES,
) -> Result<ID3D12Resource> {
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
        MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
        CreationNodeMask: 0,
        VisibleNodeMask: 0,
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: width.max(1) as u64,
        Height: height.max(1),
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: flags,
    };
    let mut res = None;
    unsafe {
        device
            .CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                initial_state,
                clear_value.map(|c| c as *const D3D12_CLEAR_VALUE),
                &mut res,
            )
            .context("CreateCommittedResource(Texture2D) failed")?;
    }
    res.context("CreateCommittedResource(Texture2D) returned None")
}

fn populate_ui_samplers(device: &ID3D12Device, heap: &ID3D12DescriptorHeap, inc: u32) {
    let filters = [
        D3D12_FILTER_MIN_MAG_MIP_LINEAR,
        D3D12_FILTER_MIN_MAG_MIP_POINT,
    ];
    let wraps = [
        D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        D3D12_TEXTURE_ADDRESS_MODE_WRAP,
        D3D12_TEXTURE_ADDRESS_MODE_MIRROR,
    ];
    let mut idx = 0u32;
    for filter in filters {
        for wrap in wraps {
            let desc = D3D12_SAMPLER_DESC {
                Filter: filter,
                AddressU: wrap,
                AddressV: wrap,
                AddressW: wrap,
                MipLODBias: 0.0,
                MaxAnisotropy: 1,
                ComparisonFunc: D3D12_COMPARISON_FUNC_NEVER,
                BorderColor: [0.0; 4],
                MinLOD: 0.0,
                MaxLOD: f32::MAX,
            };
            unsafe {
                device.CreateSampler(&desc, cpu_handle_at(heap, inc, idx));
            }
            idx += 1;
        }
    }
}

fn compile_all_shaders() -> Result<CompiledShaders> {
    let nv12_src = format!("{HLSL_COLOR_CAS_HELPERS}\n{NV12_ARRAY_PS_BODY}");
    let p010_src = format!("{HLSL_COLOR_CAS_HELPERS}\n{P010_ARRAY_PS_BODY}");
    let yuv_src = format!("{HLSL_COLOR_CAS_HELPERS}\n{YUV420_PS_BODY}");
    Ok(CompiledShaders {
        video_vs: compile_hlsl(VIDEO_VS_HLSL, s!("VSMain"), s!("vs_5_0"))?,
        ps_nv12: compile_hlsl(&nv12_src, s!("PSMain"), s!("ps_5_0"))?,
        ps_p010: compile_hlsl(&p010_src, s!("PSMain"), s!("ps_5_0"))?,
        ps_yuv420: compile_hlsl(&yuv_src, s!("PSMain"), s!("ps_5_0"))?,
        ps_fsr_easu: compile_hlsl(FSR_EASU_PS_HLSL, s!("PSMain"), s!("ps_5_0"))?,
        ps_fsr_rcas: compile_hlsl(FSR_RCAS_PS_HLSL, s!("PSMain"), s!("ps_5_0"))?,
        ui_vs: compile_hlsl(UI_VS_HLSL, s!("VSMain"), s!("vs_5_0"))?,
        ui_ps: compile_hlsl(UI_PS_HLSL, s!("PSMain"), s!("ps_5_0"))?,
    })
}

fn create_root_signatures(
    device: &ID3D12Device,
) -> Result<(ID3D12RootSignature, ID3D12RootSignature)> {
    // Video root signature: b0 (VS), b0 (PS), SRV table t0..t2 (PS), static linear clamp s0 (PS).
    let srv_range = D3D12_DESCRIPTOR_RANGE {
        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
        NumDescriptors: 3,
        BaseShaderRegister: 0,
        RegisterSpace: 0,
        OffsetInDescriptorsFromTableStart: 0,
    };
    let video_params = [
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_CBV,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Descriptor: D3D12_ROOT_DESCRIPTOR {
                    ShaderRegister: 0,
                    RegisterSpace: 0,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_VERTEX,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_CBV,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Descriptor: D3D12_ROOT_DESCRIPTOR {
                    ShaderRegister: 0,
                    RegisterSpace: 0,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: &srv_range,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
        },
    ];
    let static_sampler = D3D12_STATIC_SAMPLER_DESC {
        Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR,
        AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D12_COMPARISON_FUNC_NEVER,
        BorderColor: D3D12_STATIC_BORDER_COLOR_TRANSPARENT_BLACK,
        MinLOD: 0.0,
        MaxLOD: f32::MAX,
        ShaderRegister: 0,
        RegisterSpace: 0,
        ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
    };
    let video_desc = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: video_params.len() as u32,
        pParameters: video_params.as_ptr(),
        NumStaticSamplers: 1,
        pStaticSamplers: &static_sampler,
        Flags: D3D12_ROOT_SIGNATURE_FLAG_NONE,
    };
    let video_sig = serialize_and_create_root_signature(device, &video_desc)?;

    // UI root signature: 4x32-bit constants b0 (ALL), SRV table t0 (PS), sampler table s0 (PS).
    let ui_srv_range = D3D12_DESCRIPTOR_RANGE {
        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
        NumDescriptors: 1,
        BaseShaderRegister: 0,
        RegisterSpace: 0,
        OffsetInDescriptorsFromTableStart: 0,
    };
    let ui_sampler_range = D3D12_DESCRIPTOR_RANGE {
        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER,
        NumDescriptors: 1,
        BaseShaderRegister: 0,
        RegisterSpace: 0,
        OffsetInDescriptorsFromTableStart: 0,
    };
    let ui_params = [
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Constants: D3D12_ROOT_CONSTANTS {
                    ShaderRegister: 0,
                    RegisterSpace: 0,
                    Num32BitValues: 4,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: &ui_srv_range,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: &ui_sampler_range,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
        },
    ];
    let ui_desc = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: ui_params.len() as u32,
        pParameters: ui_params.as_ptr(),
        NumStaticSamplers: 0,
        pStaticSamplers: std::ptr::null(),
        Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
    };
    let ui_sig = serialize_and_create_root_signature(device, &ui_desc)?;
    Ok((video_sig, ui_sig))
}

fn serialize_and_create_root_signature(
    device: &ID3D12Device,
    desc: &D3D12_ROOT_SIGNATURE_DESC,
) -> Result<ID3D12RootSignature> {
    let mut blob = None;
    let mut err_blob = None;
    unsafe {
        if let Err(e) = D3D12SerializeRootSignature(
            desc,
            D3D_ROOT_SIGNATURE_VERSION_1_0,
            &mut blob,
            Some(&mut err_blob),
        ) {
            let msg = err_blob
                .as_ref()
                .map(|b| String::from_utf8_lossy(blob_slice(b)).into_owned())
                .unwrap_or_default();
            bail!("D3D12SerializeRootSignature failed ({e}): {msg}");
        }
        let blob = blob.context("D3D12SerializeRootSignature returned None")?;
        device
            .CreateRootSignature(0, blob_slice(&blob))
            .context("CreateRootSignature failed")
    }
}

fn create_pipeline_set(
    device: &ID3D12Device,
    video_root_sig: &ID3D12RootSignature,
    ui_root_sig: &ID3D12RootSignature,
    shaders: &CompiledShaders,
    rtv_format: DXGI_FORMAT,
) -> Result<PipelineFormatSet> {
    let vs = bytecode(&shaders.video_vs);
    Ok(PipelineFormatSet {
        ps_nv12: create_pso(
            device,
            video_root_sig,
            vs,
            bytecode(&shaders.ps_nv12),
            &[],
            false,
            rtv_format,
        )?,
        ps_p010: create_pso(
            device,
            video_root_sig,
            vs,
            bytecode(&shaders.ps_p010),
            &[],
            false,
            rtv_format,
        )?,
        ps_yuv420: create_pso(
            device,
            video_root_sig,
            vs,
            bytecode(&shaders.ps_yuv420),
            &[],
            false,
            rtv_format,
        )?,
        ps_fsr_easu: create_pso(
            device,
            video_root_sig,
            vs,
            bytecode(&shaders.ps_fsr_easu),
            &[],
            false,
            rtv_format,
        )?,
        ps_fsr_rcas: create_pso(
            device,
            video_root_sig,
            vs,
            bytecode(&shaders.ps_fsr_rcas),
            &[],
            false,
            rtv_format,
        )?,
        ui: create_pso(
            device,
            ui_root_sig,
            bytecode(&shaders.ui_vs),
            bytecode(&shaders.ui_ps),
            &[
                D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: s!("POSITION"),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 0,
                    InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
                D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: s!("TEXCOORD"),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 8,
                    InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
                D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: s!("COLOR"),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    InputSlot: 0,
                    AlignedByteOffset: 16,
                    InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
            ],
            true,
            rtv_format,
        )?,
    })
}

fn create_pso(
    device: &ID3D12Device,
    root_sig: &ID3D12RootSignature,
    vs: D3D12_SHADER_BYTECODE,
    ps: D3D12_SHADER_BYTECODE,
    input_elements: &[D3D12_INPUT_ELEMENT_DESC],
    alpha_blend: bool,
    rtv_format: DXGI_FORMAT,
) -> Result<ID3D12PipelineState> {
    let mut blend = D3D12_BLEND_DESC::default();
    blend.RenderTarget[0] = D3D12_RENDER_TARGET_BLEND_DESC {
        BlendEnable: alpha_blend.into(),
        LogicOpEnable: false.into(),
        SrcBlend: D3D12_BLEND_ONE,
        DestBlend: if alpha_blend {
            D3D12_BLEND_INV_SRC_ALPHA
        } else {
            windows::Win32::Graphics::Direct3D12::D3D12_BLEND_ZERO
        },
        BlendOp: D3D12_BLEND_OP_ADD,
        SrcBlendAlpha: if alpha_blend {
            D3D12_BLEND_INV_DEST_ALPHA
        } else {
            D3D12_BLEND_ONE
        },
        DestBlendAlpha: if alpha_blend {
            D3D12_BLEND_ONE
        } else {
            windows::Win32::Graphics::Direct3D12::D3D12_BLEND_ZERO
        },
        BlendOpAlpha: D3D12_BLEND_OP_ADD,
        LogicOp: D3D12_LOGIC_OP_NOOP,
        RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
    };
    let rasterizer = D3D12_RASTERIZER_DESC {
        FillMode: D3D12_FILL_MODE_SOLID,
        CullMode: D3D12_CULL_MODE_NONE,
        DepthClipEnable: true.into(),
        ..Default::default()
    };
    let stencil_op = D3D12_DEPTH_STENCILOP_DESC {
        StencilFailOp: D3D12_STENCIL_OP_KEEP,
        StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
        StencilPassOp: D3D12_STENCIL_OP_KEEP,
        StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
    };
    let depth_stencil = D3D12_DEPTH_STENCIL_DESC {
        DepthEnable: false.into(),
        DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ZERO,
        DepthFunc: D3D12_COMPARISON_FUNC_ALWAYS,
        StencilEnable: false.into(),
        StencilReadMask: 0,
        StencilWriteMask: 0,
        FrontFace: stencil_op,
        BackFace: stencil_op,
    };
    let mut rtv_formats = [DXGI_FORMAT_UNKNOWN; 8];
    rtv_formats[0] = rtv_format;

    let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
        pRootSignature: ManuallyDrop::new(Some(root_sig.clone())),
        VS: vs,
        PS: ps,
        BlendState: blend,
        SampleMask: u32::MAX,
        RasterizerState: rasterizer,
        DepthStencilState: depth_stencil,
        InputLayout: D3D12_INPUT_LAYOUT_DESC {
            pInputElementDescs: if input_elements.is_empty() {
                std::ptr::null()
            } else {
                input_elements.as_ptr()
            },
            NumElements: input_elements.len() as u32,
        },
        IBStripCutValue: D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_DISABLED,
        PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
        NumRenderTargets: 1,
        RTVFormats: rtv_formats,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        ..Default::default()
    };
    let res = unsafe { device.CreateGraphicsPipelineState(&desc) };
    unsafe {
        ManuallyDrop::drop(&mut desc.pRootSignature);
    }
    res.context("CreateGraphicsPipelineState failed")
}

fn bytecode(blob: &ID3DBlob) -> D3D12_SHADER_BYTECODE {
    let slice = blob_slice(blob);
    D3D12_SHADER_BYTECODE {
        pShaderBytecode: slice.as_ptr().cast(),
        BytecodeLength: slice.len(),
    }
}

fn array_plane_srv_desc(
    format: DXGI_FORMAT,
    slice: u32,
    plane: u32,
) -> D3D12_SHADER_RESOURCE_VIEW_DESC {
    D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2DARRAY,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2DArray: D3D12_TEX2D_ARRAY_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
                FirstArraySlice: slice,
                ArraySize: 1,
                PlaneSlice: plane,
                ResourceMinLODClamp: 0.0,
            },
        },
    }
}

fn tex2d_srv_desc(format: DXGI_FORMAT) -> D3D12_SHADER_RESOURCE_VIEW_DESC {
    D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D12_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
                PlaneSlice: 0,
                ResourceMinLODClamp: 0.0,
            },
        },
    }
}

fn null_tex2d_srv_desc() -> D3D12_SHADER_RESOURCE_VIEW_DESC {
    tex2d_srv_desc(DXGI_FORMAT_R8_UNORM)
}

fn cpu_handle_at(
    heap: &ID3D12DescriptorHeap,
    increment: u32,
    index: u32,
) -> D3D12_CPU_DESCRIPTOR_HANDLE {
    let start = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    D3D12_CPU_DESCRIPTOR_HANDLE {
        ptr: start.ptr + (index as usize) * (increment as usize),
    }
}

fn gpu_handle_at(
    heap: &ID3D12DescriptorHeap,
    increment: u32,
    index: u32,
) -> D3D12_GPU_DESCRIPTOR_HANDLE {
    let start = unsafe { heap.GetGPUDescriptorHandleForHeapStart() };
    D3D12_GPU_DESCRIPTOR_HANDLE {
        ptr: start.ptr + u64::from(index) * u64::from(increment),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_up_256_and_512_math() {
        assert_eq!(align_up(0, 256), 0);
        assert_eq!(align_up(64, 256), 256);
        assert_eq!(align_up(256, 256), 256);
        assert_eq!(align_up(257, 256), 512);
        assert_eq!(align_up(1, 512), 512);
        assert_eq!(align_up(512, 512), 512);
        assert_eq!(align_up(513, 512), 1024);
    }

    #[test]
    fn d3d12_subresource_calc_matches_nv12_array_and_tex2d() {
        assert_eq!(calc_plane_subresource(0, 0, 1, 1), 0);
        assert_eq!(calc_plane_subresource(0, 1, 1, 1), 1);
        assert_eq!(calc_plane_subresource(3, 0, 1, 8), 3);
        assert_eq!(calc_plane_subresource(3, 1, 1, 8), 11);
    }

    #[test]
    fn headless_d3d12_device_compiles_video_and_ui_psos() {
        let Ok((_factory, device)) = create_d3d12_device() else {
            return;
        };
        let shaders = compile_all_shaders().expect("video + UI HLSL shaders compile");
        let (video_sig, ui_sig) = create_root_signatures(&device).expect("root signatures create");
        let _sdr = create_pipeline_set(
            &device,
            &video_sig,
            &ui_sig,
            &shaders,
            DXGI_FORMAT_B8G8R8A8_UNORM,
        )
        .expect("SDR D3D12 PSO set creates");
        let _hdr = create_pipeline_set(
            &device,
            &video_sig,
            &ui_sig,
            &shaders,
            DXGI_FORMAT_R10G10B10A2_UNORM,
        )
        .expect("HDR10 D3D12 PSO set creates");
    }
}
