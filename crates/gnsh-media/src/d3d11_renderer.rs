pub use crate::renderer::ScaleMode;
use crate::renderer::{VideoFilterMode, VideoImageAdjustments, VideoRenderOptions, VideoRenderer};
use crate::video_decoder::{D3D11Frame, DecodedFrame, FrameColorInfo, YuvColorMatrix};
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
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BUFFER_DESC, D3D11_COMPARISON_NEVER, D3D11_CPU_ACCESS_WRITE,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_MAP_WRITE_DISCARD, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SAMPLER_DESC, D3D11_SDK_VERSION, D3D11_SHADER_RESOURCE_VIEW_DESC,
    D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_SUBRESOURCE_DATA, D3D11_TEX2D_ARRAY_SRV,
    D3D11_TEX2D_SRV, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_DYNAMIC, D3D11_VIEWPORT, D3D11CreateDevice, ID3D11Buffer, ID3D11Device,
    ID3D11DeviceContext, ID3D11Multithread, ID3D11PixelShader, ID3D11RenderTargetView,
    ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_P010, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM,
    DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_FEATURE_PRESENT_ALLOW_TEARING, DXGI_PRESENT, DXGI_PRESENT_ALLOW_TEARING,
    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIDevice1,
    IDXGIFactory2, IDXGIFactory5, IDXGIOutput6, IDXGISwapChain1, IDXGISwapChain2, IDXGISwapChain3,
};
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use windows::core::{BOOL, Interface as _, PCSTR, s};

pub(crate) const VIDEO_VS_HLSL: &str = r#"
cbuffer VideoParams : register(b0) {
    float4 pos_scale;     // xy = scale, zw = offset
    float4 uv_rect;       // xy = active scale, zw = offset
    float4 uv_clamp;      // xy = luma max uv, zw = chroma max uv
    float4 sharp_params;  // x = sharpness (0..1), y = 1/tex_w, z = 1/tex_h, w = color mode
    float4 color_grade;   // x = vibrance, y = contrast, z = gamma, w = hdr_sdr_exposure
    float4 signal_params; // x = matrix (0=709,1=601,2=2020), y = full_range, z = deband, w = chroma_left_u
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

pub(crate) const HLSL_COLOR_CAS_HELPERS: &str = r#"
float3 cas_filter(float3 c, float3 n, float3 s, float3 w, float3 e, float sharpness) {
    float3 min_c = min(c, min(min(n, s), min(w, e)));
    float3 max_c = max(c, max(max(n, s), max(w, e)));
    float3 amp = sqrt(saturate(min(min_c, 1.0 - max_c) / max(max_c, 1e-4)));
    float peak = -1.0 / lerp(8.0, 5.0, saturate(sharpness));
    float3 weight = amp * peak;
    float3 sharp = saturate((c + (n + s + w + e) * weight) / (1.0 + 4.0 * weight));
    return lerp(c, sharp, saturate(sharpness));
}

float deband_hash(float2 pos) {
    return frac(sin(dot(pos, float2(12.9898, 78.233))) * 43758.5453);
}

float3 deband_blend(float3 c, float3 s0, float3 s1, float3 s2, float3 s3, float threshold, float h) {
    float3 avg = 0.25 * (s0 + s1 + s2 + s3);
    float3 delta = abs(c - avg);
    float diff = max(delta.r, max(delta.g, delta.b));
    float w = smoothstep(threshold, 0.0, diff);
    return saturate(lerp(c, avg, w) + (h - 0.5) / 255.0);
}

float3 bt2020_yuv_to_pq(float y, float cb, float cr) {
    float r = y + 1.4746 * cr;
    float g = y - 0.1645531268 * cb - 0.5713531268 * cr;
    float b = y + 1.8814 * cb;
    return saturate(float3(r, g, b));
}

float3 bt709_yuv_to_srgb(float y, float cb, float cr) {
    float r = y + 1.5748 * cr;
    float g = y - 0.187324 * cb - 0.468124 * cr;
    float b = y + 1.8556 * cb;
    return saturate(float3(r, g, b));
}

float3 bt601_yuv_to_srgb(float y, float cb, float cr) {
    float r = y + 1.402 * cr;
    float g = y - 0.344136 * cb - 0.714136 * cr;
    float b = y + 1.772 * cb;
    return saturate(float3(r, g, b));
}

float3 yuv_to_rgb_matrix(float y, float cb, float cr, float matrix_id) {
    if (matrix_id > 1.5) {
        return bt2020_yuv_to_pq(y, cb, cr);
    }
    if (matrix_id > 0.5) {
        return bt601_yuv_to_srgb(y, cb, cr);
    }
    return bt709_yuv_to_srgb(y, cb, cr);
}

float3 apply_image_grade(float3 c, float4 grade) {
    if (abs(grade.z - 1.0) > 1e-4) {
        c = pow(max(c, 0.0), 1.0 / grade.z);
    }
    if (abs(grade.y - 1.0) > 1e-4) {
        c = saturate((c - 0.5) * grade.y + 0.5);
    }
    if (abs(grade.x) > 1e-4) {
        float luma = dot(c, float3(0.2126, 0.7152, 0.0722));
        float sat = max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b));
        c = saturate(lerp(float3(luma, luma, luma), c, 1.0 + grade.x * (1.0 - sat)));
    }
    return saturate(c);
}

float3 srgb_to_linear(float3 c) {
    float3 lo = c / 12.92;
    float3 hi = pow(max((c + 0.055) / 1.055, 0.0), 2.4);
    return (c <= 0.04045) ? lo : hi;
}

float3 linear_to_srgb(float3 c) {
    float3 lo = c * 12.92;
    float3 hi = 1.055 * pow(max(c, 0.0), 1.0 / 2.4) - 0.055;
    return (c <= 0.0031308) ? lo : hi;
}

float3 pq_to_linear(float3 pq) {
    float3 p = pow(max(pq, 0.0), 1.0 / 78.84375);
    float3 num = max(p - 0.8359375, 0.0);
    float3 den = max(18.8515625 - 18.6875 * p, 1e-6);
    return pow(num / den, 1.0 / 0.1593017578125);
}

float3 linear_to_pq(float3 lin) {
    float3 ym = pow(saturate(lin), 0.1593017578125);
    return pow((0.8359375 + 18.8515625 * ym) / (1.0 + 18.6875 * ym), 78.84375);
}

float3 srgb_to_bt2020_pq(float3 srgb, float white_nits) {
    float3 lin709 = srgb_to_linear(saturate(srgb));
    float3 lin2020 = float3(
        0.627404 * lin709.r + 0.329283 * lin709.g + 0.043313 * lin709.b,
        0.069097 * lin709.r + 0.919540 * lin709.g + 0.011363 * lin709.b,
        0.016391 * lin709.r + 0.088013 * lin709.g + 0.895596 * lin709.b
    );
    return linear_to_pq(lin2020 * (white_nits / 10000.0));
}

float3 pq_bt2020_to_sdr_srgb(float3 pq, float exposure) {
    float3 lin2020 = pq_to_linear(pq) * (40.0 * max(exposure, 0.1));
    float3 lin709 = max(float3(
         1.660491 * lin2020.r - 0.587641 * lin2020.g - 0.072850 * lin2020.b,
        -0.124550 * lin2020.r + 1.132900 * lin2020.g - 0.008350 * lin2020.b,
        -0.018151 * lin2020.r - 0.100579 * lin2020.g + 1.118730 * lin2020.b
    ), 0.0);
    float3 aces = (lin709 * (2.51 * lin709 + 0.03)) / (lin709 * (2.43 * lin709 + 0.59) + 0.14);
    return saturate(linear_to_srgb(aces));
}
"#;

pub(crate) const NV12_ARRAY_PS_BODY: &str = r#"
Texture2DArray<float> texY  : register(t0);
Texture2DArray<float2> texUV : register(t1);
SamplerState sampLinear : register(s0);
cbuffer VideoParams : register(b0) {
    float4 pos_scale;
    float4 uv_rect;
    float4 uv_clamp;
    float4 sharp_params;
    float4 color_grade;
    float4 signal_params;
};

struct PSIn {
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};

float3 sample_nv12(float2 uv) {
    float2 uv_y = clamp(uv, float2(0.0, 0.0), uv_clamp.xy);
    float2 uv_c = clamp(float2(uv.x + signal_params.w, uv.y), float2(0.0, 0.0), uv_clamp.zw);
    float y_raw = texY.SampleLevel(sampLinear, float3(uv_y, 0.0), 0.0);
    float2 uv_raw = texUV.SampleLevel(sampLinear, float3(uv_c, 0.0), 0.0);
    float y = (signal_params.y > 0.5) ? y_raw : ((y_raw - 16.0 / 255.0) * (255.0 / 219.0));
    float cb = (signal_params.y > 0.5) ? (uv_raw.x - 128.0 / 255.0) : ((uv_raw.x - 128.0 / 255.0) * (255.0 / 224.0));
    float cr = (signal_params.y > 0.5) ? (uv_raw.y - 128.0 / 255.0) : ((uv_raw.y - 128.0 / 255.0) * (255.0 / 224.0));
    return yuv_to_rgb_matrix(y, cb, cr, signal_params.x);
}

float4 PSMain(PSIn i) : SV_TARGET {
    float3 c = sample_nv12(i.uv);
    [branch]
    if (signal_params.z > 0.5) {
        float h0 = deband_hash(i.pos.xy);
        float h1 = deband_hash(i.pos.xy + float2(17.0, 31.0));
        float angle = h0 * 6.2831853;
        float radius = lerp(3.0, 10.0, h1);
        float2 off = float2(cos(angle), sin(angle)) * radius * sharp_params.yz;
        float2 off_p = float2(-off.y, off.x);
        c = deband_blend(
            c,
            sample_nv12(i.uv + off),
            sample_nv12(i.uv - off),
            sample_nv12(i.uv + off_p),
            sample_nv12(i.uv - off_p),
            14.0 / 255.0,
            h0
        );
    }
    [branch]
    if (sharp_params.x > 0.001) {
        float2 dx = float2(sharp_params.y, 0.0);
        float2 dy = float2(0.0, sharp_params.z);
        c = cas_filter(
            c,
            sample_nv12(i.uv - dy),
            sample_nv12(i.uv + dy),
            sample_nv12(i.uv - dx),
            sample_nv12(i.uv + dx),
            sharp_params.x
        );
    }
    c = apply_image_grade(c, color_grade);
    if (sharp_params.w > 0.5) {
        c = srgb_to_bt2020_pq(c, 200.0);
    }
    return float4(c, 1.0);
}
"#;

pub(crate) const P010_ARRAY_PS_BODY: &str = r#"
Texture2DArray<float> texY  : register(t0);
Texture2DArray<float2> texUV : register(t1);
SamplerState sampLinear : register(s0);
cbuffer VideoParams : register(b0) {
    float4 pos_scale;
    float4 uv_rect;
    float4 uv_clamp;
    float4 sharp_params;
    float4 color_grade;
    float4 signal_params;
};

struct PSIn {
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};

float3 sample_p010(float2 uv) {
    float2 uv_y = clamp(uv, float2(0.0, 0.0), uv_clamp.xy);
    float2 uv_c = clamp(float2(uv.x + signal_params.w, uv.y), float2(0.0, 0.0), uv_clamp.zw);
    float y_raw = texY.SampleLevel(sampLinear, float3(uv_y, 0.0), 0.0);
    float2 uv_raw = texUV.SampleLevel(sampLinear, float3(uv_c, 0.0), 0.0);
    float y = (signal_params.y > 0.5) ? y_raw : ((y_raw - 4096.0 / 65535.0) * (65535.0 / 56064.0));
    float cb = (signal_params.y > 0.5) ? (uv_raw.x - 32768.0 / 65535.0) : ((uv_raw.x - 32768.0 / 65535.0) * (65535.0 / 57344.0));
    float cr = (signal_params.y > 0.5) ? (uv_raw.y - 32768.0 / 65535.0) : ((uv_raw.y - 32768.0 / 65535.0) * (65535.0 / 57344.0));
    return bt2020_yuv_to_pq(y, cb, cr);
}

float4 PSMain(PSIn i) : SV_TARGET {
    float3 c = sample_p010(i.uv);
    [branch]
    if (signal_params.z > 0.5) {
        float h0 = deband_hash(i.pos.xy);
        float h1 = deband_hash(i.pos.xy + float2(17.0, 31.0));
        float angle = h0 * 6.2831853;
        float radius = lerp(3.0, 10.0, h1);
        float2 off = float2(cos(angle), sin(angle)) * radius * sharp_params.yz;
        float2 off_p = float2(-off.y, off.x);
        c = deband_blend(
            c,
            sample_p010(i.uv + off),
            sample_p010(i.uv - off),
            sample_p010(i.uv + off_p),
            sample_p010(i.uv - off_p),
            10.0 / 255.0,
            h0
        );
    }
    [branch]
    if (sharp_params.x > 0.001) {
        float2 dx = float2(sharp_params.y, 0.0);
        float2 dy = float2(0.0, sharp_params.z);
        c = cas_filter(
            c,
            sample_p010(i.uv - dy),
            sample_p010(i.uv + dy),
            sample_p010(i.uv - dx),
            sample_p010(i.uv + dx),
            sharp_params.x
        );
    }
    if (sharp_params.w > 0.5) {
        return float4(c, 1.0);
    }
    return float4(apply_image_grade(pq_bt2020_to_sdr_srgb(c, color_grade.w), color_grade), 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct VideoParams {
    pub(crate) pos_scale: [f32; 4],
    pub(crate) uv_rect: [f32; 4],
    pub(crate) uv_clamp: [f32; 4],
    // x = sharpness (0..=1), y = 1.0/tex_w, z = 1.0/tex_h, w = color mode flag
    pub(crate) sharp_params: [f32; 4],
    // x = vibrance, y = contrast, z = gamma, w = hdr_sdr_exposure
    pub(crate) color_grade: [f32; 4],
    // x = matrix (0=709,1=601,2=2020), y = full_range, z = deband, w = chroma_left_u
    pub(crate) signal_params: [f32; 4],
}

impl Default for VideoParams {
    fn default() -> Self {
        Self {
            pos_scale: [1.0, 1.0, 0.0, 0.0],
            uv_rect: [1.0, 1.0, 0.0, 0.0],
            uv_clamp: [1.0, 1.0, 1.0, 1.0],
            sharp_params: [0.0, 0.0, 0.0, 0.0],
            color_grade: [0.0, 1.0, 1.0, 1.0],
            signal_params: [0.0, 0.0, 0.0, 0.0],
        }
    }
}

pub(crate) const YUV420_PS_BODY: &str = r#"
Texture2D<float> texY : register(t0);
Texture2D<float> texU : register(t1);
Texture2D<float> texV : register(t2);
SamplerState sampLinear : register(s0);
cbuffer VideoParams : register(b0) {
    float4 pos_scale;
    float4 uv_rect;
    float4 uv_clamp;
    float4 sharp_params;
    float4 color_grade;
    float4 signal_params;
};

struct PSIn {
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};

float3 sample_yuv420(float2 uv, bool is_hdr_src) {
    float2 uvy = saturate(uv);
    float2 uvc = saturate(float2(uv.x + signal_params.w, uv.y));
    float y_raw = texY.SampleLevel(sampLinear, uvy, 0.0);
    float u_raw = texU.SampleLevel(sampLinear, uvc, 0.0);
    float v_raw = texV.SampleLevel(sampLinear, uvc, 0.0);
    float y = (signal_params.y > 0.5) ? y_raw : ((y_raw - 16.0 / 255.0) * (255.0 / 219.0));
    float cb = (signal_params.y > 0.5) ? (u_raw - 128.0 / 255.0) : ((u_raw - 128.0 / 255.0) * (255.0 / 224.0));
    float cr = (signal_params.y > 0.5) ? (v_raw - 128.0 / 255.0) : ((v_raw - 128.0 / 255.0) * (255.0 / 224.0));
    float mat_id = is_hdr_src ? 2.0 : signal_params.x;
    return yuv_to_rgb_matrix(y, cb, cr, mat_id);
}

float4 PSMain(PSIn i) : SV_TARGET {
    // mode: 0 = SDR->SDR, 1 = HDR->SDR (ACES), 2 = HDR->HDR (PQ), 3 = SDR->HDR (200 nits)
    bool is_hdr_src = (sharp_params.w > 0.5 && sharp_params.w < 2.5);
    float3 c = sample_yuv420(i.uv, is_hdr_src);
    [branch]
    if (signal_params.z > 0.5) {
        float h0 = deband_hash(i.pos.xy);
        float h1 = deband_hash(i.pos.xy + float2(17.0, 31.0));
        float angle = h0 * 6.2831853;
        float radius = lerp(3.0, 10.0, h1);
        float2 off = float2(cos(angle), sin(angle)) * radius * sharp_params.yz;
        float2 off_p = float2(-off.y, off.x);
        float thresh = is_hdr_src ? (10.0 / 255.0) : (14.0 / 255.0);
        c = deband_blend(
            c,
            sample_yuv420(i.uv + off, is_hdr_src),
            sample_yuv420(i.uv - off, is_hdr_src),
            sample_yuv420(i.uv + off_p, is_hdr_src),
            sample_yuv420(i.uv - off_p, is_hdr_src),
            thresh,
            h0
        );
    }
    [branch]
    if (sharp_params.x > 0.001) {
        float2 dx = float2(sharp_params.y, 0.0);
        float2 dy = float2(0.0, sharp_params.z);
        c = cas_filter(
            c,
            sample_yuv420(i.uv - dy, is_hdr_src),
            sample_yuv420(i.uv + dy, is_hdr_src),
            sample_yuv420(i.uv - dx, is_hdr_src),
            sample_yuv420(i.uv + dx, is_hdr_src),
            sharp_params.x
        );
    }
    if (sharp_params.w > 2.5) {
        return float4(srgb_to_bt2020_pq(apply_image_grade(c, color_grade), 200.0), 1.0);
    }
    if (sharp_params.w > 1.5) {
        return float4(c, 1.0);
    }
    if (sharp_params.w > 0.5) {
        return float4(apply_image_grade(pq_bt2020_to_sdr_srgb(c, color_grade.w), color_grade), 1.0);
    }
    return float4(apply_image_grade(c, color_grade), 1.0);
}
"#;

pub(crate) const FSR_EASU_PS_HLSL: &str = r#"
Texture2D texSrc : register(t0);
SamplerState sampLinear : register(s0);
cbuffer FsrParams : register(b0) {
    float4 src_size;    // x = src_w, y = src_h, z = 1/src_w, w = 1/src_h
    float4 rcas_bounds; // unused in EASU
    float4 rcas_params; // unused in EASU
};

struct PSIn {
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};

void easu_set(
    inout float2 dir,
    inout float len,
    float2 pp,
    bool biS, bool biT, bool biU, bool biV,
    float lA, float lB, float lC, float lD, float lE
) {
    float w = 0.0;
    if (biS) w = (1.0 - pp.x) * (1.0 - pp.y);
    if (biT) w = pp.x * (1.0 - pp.y);
    if (biU) w = (1.0 - pp.x) * pp.y;
    if (biV) w = pp.x * pp.y;

    float dc = lD - lC;
    float cb = lC - lB;
    float lenX = max(abs(dc), abs(cb));
    float dirX = lD - lB;
    dir.x += dirX * w;
    lenX = (lenX > 1e-5) ? saturate(abs(dirX) / lenX) : 0.0;
    lenX *= lenX;
    len += lenX * w;

    float ec = lE - lC;
    float ca = lC - lA;
    float lenY = max(abs(ec), abs(ca));
    float dirY = lE - lA;
    dir.y += dirY * w;
    lenY = (lenY > 1e-5) ? saturate(abs(dirY) / lenY) : 0.0;
    lenY *= lenY;
    len += lenY * w;
}

void easu_tap(
    inout float3 aC,
    inout float aW,
    float2 off,
    float2 dir,
    float2 len2,
    float lob,
    float clp,
    float3 c
) {
    float2 v;
    v.x = (off.x * dir.x) + (off.y * dir.y);
    v.y = (off.x * (-dir.y)) + (off.y * dir.x);
    v *= len2;
    float d2 = min(dot(v, v), clp);
    float wB = (2.0 / 5.0) * d2 - 1.0;
    float wA = lob * d2 - 1.0;
    wB *= wB;
    wA *= wA;
    wB = (25.0 / 16.0) * wB - (25.0 / 16.0 - 1.0);
    float w = wB * wA;
    aC += c * w;
    aW += w;
}

float4 PSMain(PSIn i) : SV_TARGET {
    float2 pp = i.uv * src_size.xy - 0.5;
    float2 fp = floor(pp);
    float2 f = pp - fp;
    float2 inv = src_size.zw;

    float2 p0 = (fp + float2(0.0, 1.0)) * inv;
    float2 p1 = (fp + float2(2.0, 1.0)) * inv;
    float2 p2 = (fp + float2(1.0, 0.0)) * inv;
    float2 p3 = (fp + float2(1.0, 2.0)) * inv;

    float4 ijfeR = texSrc.GatherRed(sampLinear, p0);
    float4 ijfeG = texSrc.GatherGreen(sampLinear, p0);
    float4 ijfeB = texSrc.GatherBlue(sampLinear, p0);

    float4 klhgR = texSrc.GatherRed(sampLinear, p1);
    float4 klhgG = texSrc.GatherGreen(sampLinear, p1);
    float4 klhgB = texSrc.GatherBlue(sampLinear, p1);

    float4 bcR = texSrc.GatherRed(sampLinear, p2);
    float4 bcG = texSrc.GatherGreen(sampLinear, p2);
    float4 bcB = texSrc.GatherBlue(sampLinear, p2);

    float4 noR = texSrc.GatherRed(sampLinear, p3);
    float4 noG = texSrc.GatherGreen(sampLinear, p3);
    float4 noB = texSrc.GatherBlue(sampLinear, p3);

    float4 bczzL = 0.5 * bcG + 0.25 * (bcR + bcB);
    float4 ijfeL = 0.5 * ijfeG + 0.25 * (ijfeR + ijfeB);
    float4 klhgL = 0.5 * klhgG + 0.25 * (klhgR + klhgB);
    float4 nozzL = 0.5 * noG + 0.25 * (noR + noB);

    float bL = bczzL.w; float cL = bczzL.z;
    float iL = ijfeL.x; float jL = ijfeL.y; float fL = ijfeL.z; float eL = ijfeL.w;
    float kL = klhgL.x; float lL = klhgL.y; float hL = klhgL.z; float gL = klhgL.w;
    float nL = nozzL.x; float oL = nozzL.y;

    float2 dir = float2(0.0, 0.0);
    float len = 0.0;
    easu_set(dir, len, f, true,  false, false, false, bL, eL, fL, gL, jL);
    easu_set(dir, len, f, false, true,  false, false, cL, fL, gL, hL, kL);
    easu_set(dir, len, f, false, false, true,  false, fL, iL, jL, kL, nL);
    easu_set(dir, len, f, false, false, false, true,  gL, jL, kL, lL, oL);

    float dirR = dot(dir, dir);
    bool zro = dirR < (1.0 / 32768.0);
    dirR = rsqrt(max(dirR, 1e-8));
    dir = zro ? float2(1.0, 0.0) : (dir * dirR);

    len = len * 0.5;
    len *= len;
    float stretch = dot(dir, dir) * rcp(max(abs(dir.x), abs(dir.y)));
    float2 len2 = float2(1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
    float lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len;
    float clp = rcp(lob);

    float3 cb = float3(bcR.w, bcG.w, bcB.w);
    float3 cc = float3(bcR.z, bcG.z, bcB.z);
    float3 ce = float3(ijfeR.w, ijfeG.w, ijfeB.w);
    float3 cf = float3(ijfeR.z, ijfeG.z, ijfeB.z);
    float3 cg = float3(klhgR.w, klhgG.w, klhgB.w);
    float3 ch = float3(klhgR.z, klhgG.z, klhgB.z);
    float3 ci = float3(ijfeR.x, ijfeG.x, ijfeB.x);
    float3 cj = float3(ijfeR.y, ijfeG.y, ijfeB.y);
    float3 ck = float3(klhgR.x, klhgG.x, klhgB.x);
    float3 cl = float3(klhgR.y, klhgG.y, klhgB.y);
    float3 cn = float3(noR.x, noG.x, noB.x);
    float3 co = float3(noR.y, noG.y, noB.y);

    float3 aC = float3(0.0, 0.0, 0.0);
    float aW = 0.0;
    easu_tap(aC, aW, float2( 0.0, -1.0) - f, dir, len2, lob, clp, cb);
    easu_tap(aC, aW, float2( 1.0, -1.0) - f, dir, len2, lob, clp, cc);
    easu_tap(aC, aW, float2(-1.0,  0.0) - f, dir, len2, lob, clp, ce);
    easu_tap(aC, aW, float2( 0.0,  0.0) - f, dir, len2, lob, clp, cf);
    easu_tap(aC, aW, float2( 1.0,  0.0) - f, dir, len2, lob, clp, cg);
    easu_tap(aC, aW, float2( 2.0,  0.0) - f, dir, len2, lob, clp, ch);
    easu_tap(aC, aW, float2(-1.0,  1.0) - f, dir, len2, lob, clp, ci);
    easu_tap(aC, aW, float2( 0.0,  1.0) - f, dir, len2, lob, clp, cj);
    easu_tap(aC, aW, float2( 1.0,  1.0) - f, dir, len2, lob, clp, ck);
    easu_tap(aC, aW, float2( 2.0,  1.0) - f, dir, len2, lob, clp, cl);
    easu_tap(aC, aW, float2( 0.0,  2.0) - f, dir, len2, lob, clp, cn);
    easu_tap(aC, aW, float2( 1.0,  2.0) - f, dir, len2, lob, clp, co);

    float3 min4 = min(min(cf, cg), min(cj, ck));
    float3 max4 = max(max(cf, cg), max(cj, ck));
    return float4(saturate(clamp(aC / max(aW, 1e-5), min4, max4)), 1.0);
}
"#;

pub(crate) const FSR_RCAS_PS_HLSL: &str = r#"
Texture2D texEasu : register(t0);
cbuffer FsrParams : register(b0) {
    float4 src_size;
    float4 rcas_bounds; // xy = min pixel (inclusive), zw = max pixel (inclusive)
    float4 rcas_params; // x = sharpness (0..1)
};

struct PSIn {
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};

float4 PSMain(PSIn i) : SV_TARGET {
    int2 p = int2(i.pos.xy);
    int2 lo = int2(rcas_bounds.xy);
    int2 hi = int2(rcas_bounds.zw);
    if (any(p < lo) || any(p > hi)) {
        return float4(0.0, 0.0, 0.0, 1.0);
    }

    float3 b = texEasu.Load(int3(clamp(p + int2( 0, -1), lo, hi), 0)).rgb;
    float3 d = texEasu.Load(int3(clamp(p + int2(-1,  0), lo, hi), 0)).rgb;
    float3 e = texEasu.Load(int3(p, 0)).rgb;
    float3 f = texEasu.Load(int3(clamp(p + int2( 1,  0), lo, hi), 0)).rgb;
    float3 h = texEasu.Load(int3(clamp(p + int2( 0,  1), lo, hi), 0)).rgb;

    float bL = 0.5 * b.g + 0.25 * (b.r + b.b);
    float dL = 0.5 * d.g + 0.25 * (d.r + d.b);
    float eL = 0.5 * e.g + 0.25 * (e.r + e.b);
    float fL = 0.5 * f.g + 0.25 * (f.r + f.b);
    float hL = 0.5 * h.g + 0.25 * (h.r + h.b);

    float nz = 0.25 * (bL + dL + fL + hL) - eL;
    float mxL = max(max(max(bL, dL), max(fL, hL)), eL);
    float mnL = min(min(min(bL, dL), min(fL, hL)), eL);
    nz = saturate(abs(nz) * rcp(max(mxL - mnL, 1e-5)));
    nz = -0.5 * nz + 1.0;

    float3 mn4 = min(min(b, d), min(f, h));
    float3 mx4 = max(max(b, d), max(f, h));
    float3 hitMin = -min(mn4, e) * rcp(max(4.0 * mx4, 1e-5));
    float3 hitMax = (1.0 - max(mx4, e)) * rcp(min(4.0 * mn4 - 4.0, -1e-5));
    float3 lobeRGB = max(hitMin, hitMax);
    float lobe = max(-0.1875, min(max(lobeRGB.r, max(lobeRGB.g, lobeRGB.b)), 0.0))
        * saturate(rcas_params.x)
        * nz;

    float3 col = (lobe * (b + d + f + h) + e) * rcp(4.0 * lobe + 1.0);
    return float4(saturate(col), 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct FsrParams {
    pub(crate) src_size: [f32; 4],
    pub(crate) rcas_bounds: [f32; 4],
    pub(crate) rcas_params: [f32; 4],
}

impl Default for FsrParams {
    fn default() -> Self {
        Self {
            src_size: [1.0, 1.0, 1.0, 1.0],
            rcas_bounds: [0.0, 0.0, 0.0, 0.0],
            rcas_params: [0.0, 0.0, 0.0, 0.0],
        }
    }
}

struct RenderTargetTex {
    width: u32,
    height: u32,
    _tex: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    srv: ID3D11ShaderResourceView,
}

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
struct D3d11SrvCache {
    texture_key: (usize, u32, u32, i32),
    slices: HashMap<u32, (ID3D11ShaderResourceView, ID3D11ShaderResourceView)>,
}

impl D3d11SrvCache {
    fn clear(&mut self) {
        self.texture_key = (0, 0, 0, 0);
        self.slices.clear();
    }
}

fn scale_mode_key(mode: ScaleMode) -> u8 {
    match mode {
        ScaleMode::Fit => 0,
        ScaleMode::Zoom => 1,
        ScaleMode::Stretch => 2,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct VideoParamsKey {
    fw: u32,
    fh: u32,
    tw: u32,
    th: u32,
    vw: u32,
    vh: u32,
    sharp_q: u32,
    mode_bits: u32,
    scale_mode: u8,
    matrix: u8,
    full_range: bool,
    deband: bool,
    vibrance_bits: u32,
    contrast_bits: u32,
    gamma_bits: u32,
    exposure_bits: u32,
}

pub struct D3D11Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swapchain: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    swapchain_flags: u32,
    swapchain_format: DXGI_FORMAT,
    hdr_output_active: bool,
    frame_latency_waitable: Option<HANDLE>,
    allow_tearing: bool,
    width: u32,
    height: u32,
    vs: ID3D11VertexShader,
    ps_nv12: ID3D11PixelShader,
    ps_p010: ID3D11PixelShader,
    ps_yuv420: ID3D11PixelShader,
    ps_fsr_easu: ID3D11PixelShader,
    ps_fsr_rcas: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    cb_params: ID3D11Buffer,
    cb_fsr: ID3D11Buffer,
    d3d11_srv_cache: D3d11SrvCache,
    last_video_params_key: Option<VideoParamsKey>,
    sw_textures: Option<SwYuvTextures>,
    fsr_src_rt: Option<RenderTargetTex>,
    fsr_easu_rt: Option<RenderTargetTex>,
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

        let nv12_src = format!("{HLSL_COLOR_CAS_HELPERS}\n{NV12_ARRAY_PS_BODY}");
        let p010_src = format!("{HLSL_COLOR_CAS_HELPERS}\n{P010_ARRAY_PS_BODY}");
        let yuv_src = format!("{HLSL_COLOR_CAS_HELPERS}\n{YUV420_PS_BODY}");

        let vs_blob = compile_hlsl(VIDEO_VS_HLSL, s!("VSMain"), s!("vs_5_0"))?;
        let ps_nv12_blob = compile_hlsl(&nv12_src, s!("PSMain"), s!("ps_5_0"))?;
        let ps_p010_blob = compile_hlsl(&p010_src, s!("PSMain"), s!("ps_5_0"))?;
        let ps_yuv_blob = compile_hlsl(&yuv_src, s!("PSMain"), s!("ps_5_0"))?;
        let ps_fsr_easu_blob = compile_hlsl(FSR_EASU_PS_HLSL, s!("PSMain"), s!("ps_5_0"))?;
        let ps_fsr_rcas_blob = compile_hlsl(FSR_RCAS_PS_HLSL, s!("PSMain"), s!("ps_5_0"))?;

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
        let ps_p010 = unsafe {
            let mut out = None;
            device.CreatePixelShader(blob_slice(&ps_p010_blob), None, Some(&mut out))?;
            out.context("CreatePixelShader(P010) returned None")?
        };
        let ps_yuv420 = unsafe {
            let mut out = None;
            device.CreatePixelShader(blob_slice(&ps_yuv_blob), None, Some(&mut out))?;
            out.context("CreatePixelShader(YUV420) returned None")?
        };
        let ps_fsr_easu = unsafe {
            let mut out = None;
            device.CreatePixelShader(blob_slice(&ps_fsr_easu_blob), None, Some(&mut out))?;
            out.context("CreatePixelShader(FSR EASU) returned None")?
        };
        let ps_fsr_rcas = unsafe {
            let mut out = None;
            device.CreatePixelShader(blob_slice(&ps_fsr_rcas_blob), None, Some(&mut out))?;
            out.context("CreatePixelShader(FSR RCAS) returned None")?
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

        let init_fsr = FsrParams::default();
        let cb_fsr_desc = D3D11_BUFFER_DESC {
            ByteWidth: std::mem::size_of::<FsrParams>() as u32,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let cb_fsr_sub = D3D11_SUBRESOURCE_DATA {
            pSysMem: (&init_fsr as *const FsrParams).cast(),
            ..Default::default()
        };
        let cb_fsr = unsafe {
            let mut out = None;
            device.CreateBuffer(&cb_fsr_desc, Some(&cb_fsr_sub), Some(&mut out))?;
            out.context("CreateBuffer(FSR CB) returned None")?
        };

        Ok(Self {
            device,
            context,
            swapchain,
            rtv: Some(rtv),
            swapchain_flags,
            swapchain_format: DXGI_FORMAT_B8G8R8A8_UNORM,
            hdr_output_active: false,
            frame_latency_waitable,
            allow_tearing,
            width: w,
            height: h,
            vs,
            ps_nv12,
            ps_p010,
            ps_yuv420,
            ps_fsr_easu,
            ps_fsr_rcas,
            sampler,
            cb_params,
            cb_fsr,
            d3d11_srv_cache: D3d11SrvCache::default(),
            last_video_params_key: None,
            sw_textures: None,
            fsr_src_rt: None,
            fsr_easu_rt: None,
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
                .ResizeBuffers(0, w, h, self.swapchain_format, flags)?;
        }
        self.rtv = Some(create_backbuffer_rtv(&self.device, &self.swapchain)?);
        self.width = w;
        self.height = h;
        self.last_video_params_key = None;
        self.fsr_easu_rt = None;
        Ok(())
    }

    /// Queries the DXGI output containing this swapchain to determine if Windows HDR is enabled.
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

    /// Returns whether the swapchain is currently presenting native 10-bit ST.2084 HDR10 frames.
    pub fn is_hdr_output_active(&self) -> bool {
        self.hdr_output_active
    }

    /// Switches the swapchain between native 10-bit HDR10 PQ output (when supported) and 8-bit SDR.
    pub fn set_hdr_output(&mut self, enable: bool) -> Result<bool> {
        let target_hdr = enable && self.is_display_hdr();
        if target_hdr == self.hdr_output_active {
            return Ok(self.hdr_output_active);
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

        unsafe {
            self.context.OMSetRenderTargets(None, None);
        }
        self.rtv = None;

        let flags = DXGI_SWAP_CHAIN_FLAG(self.swapchain_flags as i32);
        let switched = (|| -> Result<()> {
            unsafe {
                self.swapchain
                    .ResizeBuffers(0, self.width, self.height, target_format, flags)
                    .context("ResizeBuffers for HDR output switch failed")?;
                let sc3: IDXGISwapChain3 = self
                    .swapchain
                    .cast()
                    .context("IDXGISwapChain3 unavailable for SetColorSpace1")?;
                sc3.SetColorSpace1(target_colorspace)
                    .context("SetColorSpace1 failed")?;
            }
            Ok(())
        })();

        if let Err(err) = switched {
            if target_hdr {
                tracing::warn!("Enabling HDR10 swapchain failed, falling back to SDR: {err:#}");
                unsafe {
                    self.swapchain.ResizeBuffers(
                        0,
                        self.width,
                        self.height,
                        DXGI_FORMAT_B8G8R8A8_UNORM,
                        flags,
                    )?;
                    if let Ok(sc3) = self.swapchain.cast::<IDXGISwapChain3>() {
                        let _ = sc3.SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                    }
                }
                self.swapchain_format = DXGI_FORMAT_B8G8R8A8_UNORM;
                self.hdr_output_active = false;
                self.rtv = Some(create_backbuffer_rtv(&self.device, &self.swapchain)?);
                self.last_video_params_key = None;
                return Ok(false);
            }
            self.swapchain_format = DXGI_FORMAT_B8G8R8A8_UNORM;
            self.hdr_output_active = false;
            self.rtv = create_backbuffer_rtv(&self.device, &self.swapchain).ok();
            self.last_video_params_key = None;
            return Err(err);
        }

        self.swapchain_format = target_format;
        self.hdr_output_active = target_hdr;
        self.rtv = Some(create_backbuffer_rtv(&self.device, &self.swapchain)?);
        self.last_video_params_key = None;
        Ok(self.hdr_output_active)
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
        self.d3d11_srv_cache.clear();
        self.last_video_params_key = None;
        self.fsr_src_rt = None;
        self.fsr_easu_rt = None;
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

    pub fn draw_video_frame(
        &mut self,
        frame: &DecodedFrame,
        options: VideoRenderOptions,
    ) -> Result<()> {
        let (fw, fh, tw, th, d3d_format, color_mode) = self.inspect_frame(frame);
        let color_info = frame.color_info();
        let image = options.image.sanitized();
        let sharp = match options.filter_mode {
            VideoFilterMode::Bilinear => 0.0,
            VideoFilterMode::Cas | VideoFilterMode::Fsr1 => options.sharpness.clamp(0.0, 1.0),
        };

        if options.filter_mode == VideoFilterMode::Fsr1 {
            let upscale = should_run_fsr_easu(fw, fh, self.width, self.height, options.scale_mode);
            if upscale || sharp > 0.001 {
                match self.draw_fsr1_frame(
                    frame,
                    fw,
                    fh,
                    tw,
                    th,
                    d3d_format,
                    color_mode,
                    color_info,
                    image,
                    sharp,
                    options.scale_mode,
                    upscale,
                ) {
                    Ok(()) => return Ok(()),
                    Err(err) => {
                        tracing::warn!(
                            "FSR 1.0 pass failed, falling back to single-pass draw: {err:#}"
                        );
                        self.fsr_src_rt = None;
                        self.fsr_easu_rt = None;
                    }
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
            color_info,
            image,
            sharp,
            options.scale_mode,
            None,
        )
    }

    fn inspect_frame(
        &self,
        frame: &DecodedFrame,
    ) -> (u32, u32, u32, u32, Option<DXGI_FORMAT>, f32) {
        match frame {
            DecodedFrame::D3D11(f) => {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                unsafe {
                    f.texture.GetDesc(&mut desc);
                }
                let mode = if self.hdr_output_active { 1.0 } else { 0.0 };
                (
                    f.width,
                    f.height,
                    desc.Width,
                    desc.Height,
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
                let mode = match (*is_hdr, self.hdr_output_active) {
                    (false, false) => 0.0,
                    (true, false) => 1.0,
                    (true, true) => 2.0,
                    (false, true) => 3.0,
                };
                (*width, *height, *width, *height, None, mode)
            }
            DecodedFrame::D3D12(_) | DecodedFrame::Vulkan(_) => (0, 0, 0, 0, None, 0.0),
        }
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
        color_info: FrameColorInfo,
        image: VideoImageAdjustments,
        sharpness: f32,
        scale_mode: ScaleMode,
        target: Option<(&ID3D11RenderTargetView, u32, u32)>,
    ) -> Result<()> {
        let (target_w, target_h) = match target {
            Some((rtv, w, h)) => {
                bind_rt_target(&self.context, rtv, w, h);
                (w, h)
            }
            None => {
                self.bind_backbuffer();
                (self.width, self.height)
            }
        };

        let sharp = sharpness.clamp(0.0, 1.0);
        let sharp_q = (sharp * 1000.0).round() as u32;
        let matrix = match color_info.matrix {
            YuvColorMatrix::Bt709 => 0,
            YuvColorMatrix::Bt601 => 1,
            YuvColorMatrix::Bt2020 => 2,
        };
        let key = VideoParamsKey {
            fw,
            fh,
            tw,
            th,
            vw: target_w,
            vh: target_h,
            sharp_q,
            mode_bits: color_mode.to_bits(),
            scale_mode: scale_mode_key(scale_mode),
            matrix,
            full_range: color_info.full_range,
            deband: image.deband,
            vibrance_bits: image.vibrance.to_bits(),
            contrast_bits: image.contrast.to_bits(),
            gamma_bits: image.gamma.to_bits(),
            exposure_bits: image.hdr_sdr_exposure.to_bits(),
        };
        if self.last_video_params_key != Some(key) {
            let params = compute_video_params(
                fw, fh, tw, th, target_w, target_h, sharp, color_mode, scale_mode, color_info,
                image,
            );
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
            DecodedFrame::D3D11(d3d_frame) => {
                let fmt = d3d_format.expect("D3D11 format populated");
                self.draw_d3d11_frame(d3d_frame, tw, th, fmt)?;
            }
            DecodedFrame::D3D12(_) | DecodedFrame::Vulkan(_) => {
                bail!("D3D11Renderer cannot consume zero-copy D3D12/Vulkan frames directly");
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
            } => self.draw_sw_yuv420(y, u, v, *y_stride, *u_stride, *v_stride, *width, *height)?,
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
                self.draw_sw_yuv420(y, &u, &v, *y_stride, uv_s, uv_s, *width, *height)?;
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
                self.draw_sw_yuv420(&y8, &u8, &v8, ys, uvs, uvs, *width, *height)?;
            }
        }

        unsafe {
            self.context
                .PSSetShaderResources(0, Some(&[None, None, None]));
        }
        Ok(())
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
        color_info: FrameColorInfo,
        image: VideoImageAdjustments,
        sharp: f32,
        scale_mode: ScaleMode,
        upscale: bool,
    ) -> Result<()> {
        let vw = self.width;
        let vh = self.height;
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
            let (easu_rtv, easu_srv) =
                ensure_render_target(&self.device, &mut self.fsr_easu_rt, vw, vh)?;
            unsafe {
                self.context
                    .ClearRenderTargetView(&easu_rtv, &[0.0, 0.0, 0.0, 1.0]);
            }
            self.draw_source_to_target(
                frame,
                fw,
                fh,
                tw,
                th,
                d3d_format,
                color_mode,
                color_info,
                image,
                0.0,
                scale_mode,
                Some((&easu_rtv, vw, vh)),
            )?;
            unsafe {
                self.context.OMSetRenderTargets(None, None);
            }
            self.bind_backbuffer();
            draw_fsr_quad_pass(
                &self.context,
                &self.vs,
                &self.ps_fsr_rcas,
                &self.cb_params,
                &self.cb_fsr,
                &self.sampler,
                &easu_srv,
                [1.0, 1.0, 0.0, 0.0],
                &fsr_params,
            );
            self.last_video_params_key = None;
            return Ok(());
        }

        let (src_rtv, src_srv) = ensure_render_target(&self.device, &mut self.fsr_src_rt, fw, fh)?;
        self.draw_source_to_target(
            frame,
            fw,
            fh,
            tw,
            th,
            d3d_format,
            color_mode,
            color_info,
            image,
            0.0,
            ScaleMode::Stretch,
            Some((&src_rtv, fw, fh)),
        )?;
        unsafe {
            self.context.OMSetRenderTargets(None, None);
        }

        if sharp > 0.001 {
            let (easu_rtv, easu_srv) =
                ensure_render_target(&self.device, &mut self.fsr_easu_rt, vw, vh)?;
            unsafe {
                self.context
                    .ClearRenderTargetView(&easu_rtv, &[0.0, 0.0, 0.0, 1.0]);
            }
            bind_rt_target(&self.context, &easu_rtv, vw, vh);
            draw_fsr_quad_pass(
                &self.context,
                &self.vs,
                &self.ps_fsr_easu,
                &self.cb_params,
                &self.cb_fsr,
                &self.sampler,
                &src_srv,
                pos_scale,
                &fsr_params,
            );
            unsafe {
                self.context.OMSetRenderTargets(None, None);
            }
            self.bind_backbuffer();
            draw_fsr_quad_pass(
                &self.context,
                &self.vs,
                &self.ps_fsr_rcas,
                &self.cb_params,
                &self.cb_fsr,
                &self.sampler,
                &easu_srv,
                [1.0, 1.0, 0.0, 0.0],
                &fsr_params,
            );
        } else {
            self.bind_backbuffer();
            draw_fsr_quad_pass(
                &self.context,
                &self.vs,
                &self.ps_fsr_easu,
                &self.cb_params,
                &self.cb_fsr,
                &self.sampler,
                &src_srv,
                pos_scale,
                &fsr_params,
            );
        }

        self.last_video_params_key = None;
        Ok(())
    }

    fn draw_d3d11_frame(
        &mut self,
        frame: &D3D11Frame,
        tex_w: u32,
        tex_h: u32,
        tex_format: DXGI_FORMAT,
    ) -> Result<()> {
        let is_p010 = tex_format == DXGI_FORMAT_P010;
        let texture_key = (frame.texture.as_raw() as usize, tex_w, tex_h, tex_format.0);
        if self.d3d11_srv_cache.texture_key != texture_key {
            self.d3d11_srv_cache.slices.clear();
            self.d3d11_srv_cache.texture_key = texture_key;
        }

        let (y_srv_fmt, uv_srv_fmt) = if is_p010 {
            (DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM)
        } else {
            (DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM)
        };

        let slice_index = frame.slice_index;
        let (y_srv, uv_srv) = match self.d3d11_srv_cache.slices.entry(slice_index) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let (y, uv) = entry.get();
                (y.clone(), uv.clone())
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let y_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                    Format: y_srv_fmt,
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
                    Format: uv_srv_fmt,
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

        let ps = if is_p010 {
            &self.ps_p010
        } else {
            &self.ps_nv12
        };
        unsafe {
            self.context.PSSetShader(ps, None);
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

impl VideoRenderer for D3D11Renderer {
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

impl Drop for D3D11Renderer {
    fn drop(&mut self) {
        self.d3d11_srv_cache.clear();
        self.fsr_src_rt = None;
        self.fsr_easu_rt = None;
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

fn bind_rt_target(
    context: &ID3D11DeviceContext,
    rtv: &ID3D11RenderTargetView,
    width: u32,
    height: u32,
) {
    let vp = D3D11_VIEWPORT {
        TopLeftX: 0.0,
        TopLeftY: 0.0,
        Width: width.max(1) as f32,
        Height: height.max(1) as f32,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    };
    unsafe {
        context.RSSetViewports(Some(&[vp]));
        context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_fsr_quad_pass(
    context: &ID3D11DeviceContext,
    vs: &ID3D11VertexShader,
    ps: &ID3D11PixelShader,
    cb_params: &ID3D11Buffer,
    cb_fsr: &ID3D11Buffer,
    sampler: &ID3D11SamplerState,
    src_srv: &ID3D11ShaderResourceView,
    pos_scale: [f32; 4],
    fsr_params: &FsrParams,
) {
    let vs_params = VideoParams {
        pos_scale,
        ..VideoParams::default()
    };
    unsafe {
        context.UpdateSubresource(
            cb_params,
            0,
            None,
            (&vs_params as *const VideoParams).cast(),
            0,
            0,
        );
        context.UpdateSubresource(
            cb_fsr,
            0,
            None,
            (fsr_params as *const FsrParams).cast(),
            0,
            0,
        );
        context.IASetInputLayout(None);
        context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        context.VSSetShader(vs, None);
        context.VSSetConstantBuffers(0, Some(&[Some(cb_params.clone())]));
        context.PSSetShader(ps, None);
        context.PSSetConstantBuffers(0, Some(&[Some(cb_fsr.clone())]));
        context.PSSetSamplers(0, Some(&[Some(sampler.clone())]));
        context.PSSetShaderResources(0, Some(&[Some(src_srv.clone())]));
        context.Draw(4, 0);
        context.PSSetShaderResources(0, Some(&[None, None, None]));
    }
}

fn ensure_render_target(
    device: &ID3D11Device,
    slot: &mut Option<RenderTargetTex>,
    width: u32,
    height: u32,
) -> Result<(ID3D11RenderTargetView, ID3D11ShaderResourceView)> {
    let w = width.max(1);
    let h = height.max(1);
    if slot
        .as_ref()
        .is_none_or(|rt| rt.width != w || rt.height != h)
    {
        *slot = Some(create_fsr_render_target(device, w, h)?);
    }
    let rt = slot.as_ref().expect("render target initialized");
    Ok((rt.rtv.clone(), rt.srv.clone()))
}

fn create_fsr_render_target(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<RenderTargetTex> {
    let format = DXGI_FORMAT_R10G10B10A2_UNORM;
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width.max(1),
        Height: height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let tex = unsafe {
        let mut out = None;
        device.CreateTexture2D(&desc, None, Some(&mut out))?;
        out.context("CreateTexture2D(FSR RT) returned None")?
    };
    let rtv = unsafe {
        let mut out = None;
        device.CreateRenderTargetView(&tex, None, Some(&mut out))?;
        out.context("CreateRenderTargetView(FSR RT) returned None")?
    };
    let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
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
        out.context("CreateShaderResourceView(FSR RT) returned None")?
    };
    Ok(RenderTargetTex {
        width: width.max(1),
        height: height.max(1),
        _tex: tex,
        rtv,
        srv,
    })
}

pub(crate) fn should_run_fsr_easu(
    frame_w: u32,
    frame_h: u32,
    view_w: u32,
    view_h: u32,
    scale_mode: ScaleMode,
) -> bool {
    if frame_w == 0 || frame_h == 0 || view_w == 0 || view_h == 0 {
        return false;
    }
    let pos_scale = aspect_scale(frame_w, frame_h, view_w, view_h, scale_mode);
    let scaled_w = ((view_w as f32) * pos_scale[0]).round() as u32;
    let scaled_h = ((view_h as f32) * pos_scale[1]).round() as u32;
    scaled_w > frame_w || scaled_h > frame_h
}

pub(crate) fn compute_rcas_bounds(pos_scale: [f32; 4], view_w: u32, view_h: u32) -> [f32; 4] {
    let vw = view_w.max(1) as f32;
    let vh = view_h.max(1) as f32;
    let left_ndc = (-pos_scale[0] + pos_scale[2]).clamp(-1.0, 1.0);
    let right_ndc = (pos_scale[0] + pos_scale[2]).clamp(-1.0, 1.0);
    let top_ndc = (pos_scale[1] + pos_scale[3]).clamp(-1.0, 1.0);
    let bottom_ndc = (-pos_scale[1] + pos_scale[3]).clamp(-1.0, 1.0);

    let left_edge = ((left_ndc + 1.0) * 0.5) * vw;
    let right_edge = ((right_ndc + 1.0) * 0.5) * vw;
    let top_edge = ((1.0 - top_ndc) * 0.5) * vh;
    let bottom_edge = ((1.0 - bottom_ndc) * 0.5) * vh;

    let max_valid_x = vw - 1.0;
    let max_valid_y = vh - 1.0;
    let min_x = (left_edge - 0.5).ceil().clamp(0.0, max_valid_x);
    let max_x = (right_edge - 0.5).floor().clamp(min_x, max_valid_x);
    let min_y = (top_edge - 0.5).ceil().clamp(0.0, max_valid_y);
    let max_y = (bottom_edge - 0.5).floor().clamp(min_y, max_valid_y);
    [min_x, min_y, max_x, max_y]
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
        if dst_ptr.is_null() || dst_stride < copy_w {
            context.Unmap(tex, 0);
            bail!(
                "D3D11 Map returned invalid plane buffer (null={}, row_pitch={dst_stride}, width={copy_w})",
                dst_ptr.is_null()
            );
        }
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

pub(crate) fn unpack_nv12_chroma(
    uv: &[u8],
    uv_stride: usize,
    width: u32,
    height: u32,
) -> (Vec<u8>, Vec<u8>, usize) {
    let cw = (width as usize).div_ceil(2);
    let ch = (height as usize).div_ceil(2);
    let mut u = Vec::with_capacity(cw * ch);
    let mut v = Vec::with_capacity(cw * ch);
    for row in 0..ch {
        let start = row * uv_stride;
        let end = start + cw * 2;
        if let Some(src_row) = uv.get(start..end) {
            for pair in src_row.as_chunks::<2>().0 {
                u.push(pair[0]);
                v.push(pair[1]);
            }
        } else {
            u.resize(u.len() + cw, 128);
            v.resize(v.len() + cw, 128);
        }
    }
    (u, v, cw)
}

pub(crate) fn unpack_p010_to_yuv8(
    y: &[u8],
    uv: &[u8],
    y_stride: usize,
    uv_stride: usize,
    width: u32,
    height: u32,
) -> (Vec<u8>, Vec<u8>, Vec<u8>, usize, usize) {
    let w = width as usize;
    let h = height as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let mut y8 = Vec::with_capacity(w * h);
    for row in 0..h {
        let start = row * y_stride;
        let end = start + w * 2;
        if let Some(src_row) = y.get(start..end) {
            for pair in src_row.as_chunks::<2>().0 {
                y8.push((u16::from_le_bytes(*pair) >> 8) as u8);
            }
        } else {
            y8.resize(y8.len() + w, 0);
        }
    }
    let mut u8_buf = Vec::with_capacity(cw * ch);
    let mut v8_buf = Vec::with_capacity(cw * ch);
    for row in 0..ch {
        let start = row * uv_stride;
        let end = start + cw * 4;
        if let Some(src_row) = uv.get(start..end) {
            for quad in src_row.as_chunks::<4>().0 {
                u8_buf.push((u16::from_le_bytes([quad[0], quad[1]]) >> 8) as u8);
                v8_buf.push((u16::from_le_bytes([quad[2], quad[3]]) >> 8) as u8);
            }
        } else {
            u8_buf.resize(u8_buf.len() + cw, 128);
            v8_buf.resize(v8_buf.len() + cw, 128);
        }
    }
    (y8, u8_buf, v8_buf, w, cw)
}

pub(crate) fn aspect_scale(
    frame_w: u32,
    frame_h: u32,
    view_w: u32,
    view_h: u32,
    scale_mode: ScaleMode,
) -> [f32; 4] {
    if frame_w == 0 || frame_h == 0 || view_w == 0 || view_h == 0 {
        return [1.0, 1.0, 0.0, 0.0];
    }
    if scale_mode == ScaleMode::Stretch {
        return [1.0, 1.0, 0.0, 0.0];
    }
    let frame_aspect = frame_w as f32 / frame_h as f32;
    let view_aspect = view_w as f32 / view_h as f32;
    match scale_mode {
        ScaleMode::Fit => {
            if view_aspect > frame_aspect {
                [frame_aspect / view_aspect, 1.0, 0.0, 0.0]
            } else {
                [1.0, view_aspect / frame_aspect, 0.0, 0.0]
            }
        }
        ScaleMode::Zoom => {
            if view_aspect > frame_aspect {
                [1.0, view_aspect / frame_aspect, 0.0, 0.0]
            } else {
                [frame_aspect / view_aspect, 1.0, 0.0, 0.0]
            }
        }
        ScaleMode::Stretch => [1.0, 1.0, 0.0, 0.0],
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_video_params(
    frame_w: u32,
    frame_h: u32,
    tex_w: u32,
    tex_h: u32,
    view_w: u32,
    view_h: u32,
    sharpness: f32,
    color_mode: f32,
    scale_mode: ScaleMode,
    color_info: FrameColorInfo,
    image: VideoImageAdjustments,
) -> VideoParams {
    let pos_scale = aspect_scale(frame_w, frame_h, view_w, view_h, scale_mode);
    let tw = tex_w.max(frame_w).max(1) as f32;
    let th = tex_h.max(frame_h).max(1) as f32;
    let fw = frame_w.max(1) as f32;
    let fh = frame_h.max(1) as f32;
    let clean = image.sanitized();
    let matrix_id = match color_info.matrix {
        YuvColorMatrix::Bt709 => 0.0,
        YuvColorMatrix::Bt601 => 1.0,
        YuvColorMatrix::Bt2020 => 2.0,
    };
    VideoParams {
        pos_scale,
        uv_rect: [fw / tw, fh / th, 0.0, 0.0],
        uv_clamp: [
            (fw - 0.5).max(0.0) / tw,
            (fh - 0.5).max(0.0) / th,
            (fw - 1.0).max(0.0) / tw,
            (fh - 1.0).max(0.0) / th,
        ],
        sharp_params: [sharpness.clamp(0.0, 1.0), 1.0 / tw, 1.0 / th, color_mode],
        color_grade: [
            clean.vibrance,
            clean.contrast,
            clean.gamma,
            clean.hdr_sdr_exposure,
        ],
        signal_params: [
            matrix_id,
            if color_info.full_range { 1.0 } else { 0.0 },
            if clean.deband { 1.0 } else { 0.0 },
            0.5 / tw,
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
    let ptr = unsafe { blob.GetBufferPointer() };
    let len = unsafe { blob.GetBufferSize() };
    if ptr.is_null() || len == 0 {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(ptr.cast(), len) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_video_hlsl_shaders() {
        let nv12_src = format!("{HLSL_COLOR_CAS_HELPERS}\n{NV12_ARRAY_PS_BODY}");
        let p010_src = format!("{HLSL_COLOR_CAS_HELPERS}\n{P010_ARRAY_PS_BODY}");
        let yuv_src = format!("{HLSL_COLOR_CAS_HELPERS}\n{YUV420_PS_BODY}");

        let vs = compile_hlsl(VIDEO_VS_HLSL, s!("VSMain"), s!("vs_5_0")).expect("VS compiles");
        let ps_nv12 =
            compile_hlsl(&nv12_src, s!("PSMain"), s!("ps_5_0")).expect("NV12 PS compiles");
        let ps_p010 =
            compile_hlsl(&p010_src, s!("PSMain"), s!("ps_5_0")).expect("P010 PS compiles");
        let ps_yuv =
            compile_hlsl(&yuv_src, s!("PSMain"), s!("ps_5_0")).expect("YUV420 PS compiles");
        let ps_easu = compile_hlsl(FSR_EASU_PS_HLSL, s!("PSMain"), s!("ps_5_0"))
            .expect("FSR 1.0 EASU PS compiles");
        let ps_rcas = compile_hlsl(FSR_RCAS_PS_HLSL, s!("PSMain"), s!("ps_5_0"))
            .expect("FSR 1.0 RCAS PS compiles");

        assert!(!blob_slice(&vs).is_empty());
        assert!(!blob_slice(&ps_nv12).is_empty());
        assert!(!blob_slice(&ps_p010).is_empty());
        assert!(!blob_slice(&ps_yuv).is_empty());
        assert!(!blob_slice(&ps_easu).is_empty());
        assert!(!blob_slice(&ps_rcas).is_empty());
    }

    #[test]
    fn fsr_easu_bypass_and_rcas_bounds_math() {
        assert!(should_run_fsr_easu(1920, 1080, 2880, 1800, ScaleMode::Fit));
        assert!(!should_run_fsr_easu(1920, 1080, 1920, 1080, ScaleMode::Fit));
        assert!(!should_run_fsr_easu(1920, 1080, 1280, 720, ScaleMode::Fit));
        assert!(should_run_fsr_easu(1280, 720, 1920, 1080, ScaleMode::Fit));

        let fit_bounds = compute_rcas_bounds([1.0, 0.9, 0.0, 0.0], 2880, 1800);
        assert_eq!(fit_bounds, [0.0, 90.0, 2879.0, 1709.0]);

        let zoom_bounds = compute_rcas_bounds([10.0 / 9.0, 1.0, 0.0, 0.0], 2880, 1800);
        assert_eq!(zoom_bounds, [0.0, 0.0, 2879.0, 1799.0]);

        let stretch_bounds = compute_rcas_bounds([1.0, 1.0, 0.0, 0.0], 1920, 1080);
        assert_eq!(stretch_bounds, [0.0, 0.0, 1919.0, 1079.0]);
    }

    #[test]
    fn computes_aspect_letterbox_scale() {
        let exact = aspect_scale(1920, 1080, 1920, 1080, ScaleMode::Fit);
        assert!((exact[0] - 1.0).abs() < 1e-5 && (exact[1] - 1.0).abs() < 1e-5);

        // 16:9 stream on 21:9 ultrawide.
        let wide_fit = aspect_scale(1920, 1080, 2560, 1080, ScaleMode::Fit);
        assert!(wide_fit[0] < 1.0 && (wide_fit[1] - 1.0).abs() < 1e-5);
        let wide_zoom = aspect_scale(1920, 1080, 2560, 1080, ScaleMode::Zoom);
        assert!((wide_zoom[0] - 1.0).abs() < 1e-5 && (wide_zoom[1] - (4.0 / 3.0)).abs() < 1e-4);

        // 16:9 stream on 16:10 (2880x1800) HDR display.
        let laptop_fit = aspect_scale(1920, 1080, 2880, 1800, ScaleMode::Fit);
        assert!((laptop_fit[0] - 1.0).abs() < 1e-5 && (laptop_fit[1] - 0.9).abs() < 1e-4);
        let laptop_zoom = aspect_scale(1920, 1080, 2880, 1800, ScaleMode::Zoom);
        assert!(
            (laptop_zoom[0] - (10.0 / 9.0)).abs() < 1e-4 && (laptop_zoom[1] - 1.0).abs() < 1e-5
        );
        let laptop_stretch = aspect_scale(1920, 1080, 2880, 1800, ScaleMode::Stretch);
        assert!((laptop_stretch[0] - 1.0).abs() < 1e-5 && (laptop_stretch[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn computes_padded_surface_uv_crop_and_sharp_params() {
        let hevc = compute_video_params(
            1920,
            1080,
            1920,
            1152,
            1920,
            1080,
            1.4,
            1.0,
            ScaleMode::Fit,
            FrameColorInfo {
                matrix: YuvColorMatrix::Bt601,
                full_range: true,
            },
            VideoImageAdjustments {
                deband: true,
                vibrance: 0.35,
                contrast: 1.1,
                gamma: 0.95,
                hdr_sdr_exposure: 1.25,
            },
        );
        assert!((hevc.uv_rect[0] - 1.0).abs() < 1e-6);
        assert!((hevc.uv_rect[1] - (1080.0 / 1152.0)).abs() < 1e-6);
        assert!(hevc.uv_clamp[1] < hevc.uv_rect[1]);
        assert!(hevc.uv_clamp[3] < hevc.uv_clamp[1]);
        assert!((hevc.sharp_params[0] - 1.0).abs() < 1e-6);
        assert!((hevc.sharp_params[1] - (1.0 / 1920.0)).abs() < 1e-6);
        assert!((hevc.sharp_params[2] - (1.0 / 1152.0)).abs() < 1e-6);
        assert!((hevc.sharp_params[3] - 1.0).abs() < 1e-6);
        assert!((hevc.color_grade[0] - 0.35).abs() < 1e-6);
        assert!((hevc.color_grade[1] - 1.1).abs() < 1e-6);
        assert!((hevc.color_grade[2] - 0.95).abs() < 1e-6);
        assert!((hevc.color_grade[3] - 1.25).abs() < 1e-6);
        assert_eq!(hevc.signal_params[0], 1.0);
        assert_eq!(hevc.signal_params[1], 1.0);
        assert_eq!(hevc.signal_params[2], 1.0);
        assert!((hevc.signal_params[3] - (0.5 / 1920.0)).abs() < 1e-7);

        let h264 = compute_video_params(
            1920,
            1080,
            1920,
            1088,
            1920,
            1080,
            -0.2,
            0.0,
            ScaleMode::Stretch,
            FrameColorInfo::default(),
            VideoImageAdjustments::default(),
        );
        assert!((h264.uv_rect[1] - (1080.0 / 1088.0)).abs() < 1e-6);
        assert!((h264.sharp_params[0] - 0.0).abs() < 1e-6);
        assert_eq!(h264.signal_params[0], 0.0);
        assert_eq!(h264.signal_params[1], 0.0);
        assert_eq!(h264.signal_params[2], 0.0);
    }
}
