use anyhow::{Context, Result, bail};
use gnsh_media::d3d11_renderer::{blob_slice, compile_hlsl};
use std::collections::HashMap;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::{
    D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D11_SRV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_INDEX_BUFFER, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BIND_VERTEX_BUFFER, D3D11_BLEND_DESC, D3D11_BLEND_INV_DEST_ALPHA,
    D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BOX, D3D11_BUFFER_DESC,
    D3D11_COMPARISON_ALWAYS, D3D11_COMPARISON_NEVER, D3D11_CPU_ACCESS_WRITE, D3D11_CULL_NONE,
    D3D11_DEPTH_STENCIL_DESC, D3D11_DEPTH_STENCILOP_DESC, D3D11_DEPTH_WRITE_MASK_ZERO,
    D3D11_FILL_SOLID, D3D11_FILTER, D3D11_FILTER_MIN_LINEAR_MAG_MIP_POINT,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_FILTER_MIN_MAG_MIP_POINT,
    D3D11_FILTER_MIN_POINT_MAG_LINEAR_MIP_POINT, D3D11_INPUT_ELEMENT_DESC,
    D3D11_INPUT_PER_VERTEX_DATA, D3D11_MAP_WRITE_DISCARD, D3D11_MAPPED_SUBRESOURCE,
    D3D11_RASTERIZER_DESC, D3D11_RENDER_TARGET_BLEND_DESC, D3D11_SAMPLER_DESC,
    D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_STENCIL_OP_KEEP,
    D3D11_SUBRESOURCE_DATA, D3D11_TEX2D_SRV, D3D11_TEXTURE_ADDRESS_CLAMP,
    D3D11_TEXTURE_ADDRESS_MIRROR, D3D11_TEXTURE_ADDRESS_MODE, D3D11_TEXTURE_ADDRESS_WRAP,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC, D3D11_VIEWPORT,
    ID3D11BlendState, ID3D11Buffer, ID3D11DepthStencilState, ID3D11Device, ID3D11DeviceContext,
    ID3D11InputLayout, ID3D11PixelShader, ID3D11RasterizerState, ID3D11SamplerState,
    ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R32_UINT, DXGI_FORMAT_R32G32_FLOAT, DXGI_SAMPLE_DESC,
};
use windows::core::s;

const EGUI_VS_HLSL: &str = r#"
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

const EGUI_PS_HLSL: &str = r#"
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

#[repr(C)]
#[derive(Clone, Copy)]
struct GpuVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [u8; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ScreenParams {
    screen_size: [f32; 2],
    hdr_paper_white_nits: f32,
    _pad: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SamplerKey {
    min: egui::TextureFilter,
    mag: egui::TextureFilter,
    wrap: egui::TextureWrapMode,
}

impl From<egui::TextureOptions> for SamplerKey {
    fn from(options: egui::TextureOptions) -> Self {
        Self {
            min: options.minification,
            mag: options.magnification,
            wrap: options.wrap_mode,
        }
    }
}

struct GpuTexture {
    texture: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    size: [usize; 2],
    options: egui::TextureOptions,
}

struct DrawCmd {
    index_count: u32,
    start_index: u32,
    base_vertex: i32,
    scissor: RECT,
    srv: ID3D11ShaderResourceView,
    sampler: ID3D11SamplerState,
}

pub struct EguiD3d11Painter {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    input_layout: ID3D11InputLayout,
    blend_state: ID3D11BlendState,
    rasterizer_state: ID3D11RasterizerState,
    depth_stencil_state: ID3D11DepthStencilState,
    cb_params: ID3D11Buffer,
    vb: Option<ID3D11Buffer>,
    vb_capacity: usize,
    ib: Option<ID3D11Buffer>,
    ib_capacity: usize,
    textures: HashMap<egui::TextureId, GpuTexture>,
    samplers: HashMap<SamplerKey, ID3D11SamplerState>,
    vertices_scratch: Vec<GpuVertex>,
    indices_scratch: Vec<u32>,
}

impl EguiD3d11Painter {
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        let vs_blob = compile_hlsl(EGUI_VS_HLSL, s!("VSMain"), s!("vs_5_0"))?;
        let ps_blob = compile_hlsl(EGUI_PS_HLSL, s!("PSMain"), s!("ps_5_0"))?;

        let vs = unsafe {
            let mut out = None;
            device.CreateVertexShader(blob_slice(&vs_blob), None, Some(&mut out))?;
            out.context("CreateVertexShader(egui) returned None")?
        };
        let ps = unsafe {
            let mut out = None;
            device.CreatePixelShader(blob_slice(&ps_blob), None, Some(&mut out))?;
            out.context("CreatePixelShader(egui) returned None")?
        };

        let input_elements = [
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("POSITION"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 0,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("TEXCOORD"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 8,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("COLOR"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                InputSlot: 0,
                AlignedByteOffset: 16,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
        ];
        let input_layout = unsafe {
            let mut out = None;
            device.CreateInputLayout(&input_elements, blob_slice(&vs_blob), Some(&mut out))?;
            out.context("CreateInputLayout(egui) returned None")?
        };

        let mut blend_desc = D3D11_BLEND_DESC {
            AlphaToCoverageEnable: false.into(),
            IndependentBlendEnable: false.into(),
            ..Default::default()
        };
        blend_desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            SrcBlend: D3D11_BLEND_ONE,
            DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: D3D11_BLEND_INV_DEST_ALPHA,
            DestBlendAlpha: D3D11_BLEND_ONE,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: 0x0F,
        };
        let blend_state = unsafe {
            let mut out = None;
            device.CreateBlendState(&blend_desc, Some(&mut out))?;
            out.context("CreateBlendState(egui) returned None")?
        };

        let rasterizer_desc = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: D3D11_CULL_NONE,
            DepthClipEnable: true.into(),
            ScissorEnable: true.into(),
            ..Default::default()
        };
        let rasterizer_state = unsafe {
            let mut out = None;
            device.CreateRasterizerState(&rasterizer_desc, Some(&mut out))?;
            out.context("CreateRasterizerState(egui) returned None")?
        };

        let stencil_op = D3D11_DEPTH_STENCILOP_DESC {
            StencilFailOp: D3D11_STENCIL_OP_KEEP,
            StencilDepthFailOp: D3D11_STENCIL_OP_KEEP,
            StencilPassOp: D3D11_STENCIL_OP_KEEP,
            StencilFunc: D3D11_COMPARISON_ALWAYS,
        };
        let depth_stencil_desc = D3D11_DEPTH_STENCIL_DESC {
            DepthEnable: false.into(),
            DepthWriteMask: D3D11_DEPTH_WRITE_MASK_ZERO,
            DepthFunc: D3D11_COMPARISON_ALWAYS,
            StencilEnable: false.into(),
            StencilReadMask: 0,
            StencilWriteMask: 0,
            FrontFace: stencil_op,
            BackFace: stencil_op,
        };
        let depth_stencil_state = unsafe {
            let mut out = None;
            device.CreateDepthStencilState(&depth_stencil_desc, Some(&mut out))?;
            out.context("CreateDepthStencilState(egui) returned None")?
        };

        let init_params = ScreenParams {
            screen_size: [1.0, 1.0],
            hdr_paper_white_nits: 0.0,
            _pad: 0.0,
        };
        let cb_desc = D3D11_BUFFER_DESC {
            ByteWidth: std::mem::size_of::<ScreenParams>() as u32,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let cb_sub = D3D11_SUBRESOURCE_DATA {
            pSysMem: (&raw const init_params).cast(),
            ..Default::default()
        };
        let cb_params = unsafe {
            let mut out = None;
            device.CreateBuffer(&cb_desc, Some(&cb_sub), Some(&mut out))?;
            out.context("CreateBuffer(egui CB) returned None")?
        };

        let default_key = SamplerKey::from(egui::TextureOptions::LINEAR);
        let default_sampler = create_sampler(device, default_key)?;
        let mut samplers = HashMap::new();
        samplers.insert(default_key, default_sampler);

        Ok(Self {
            vs,
            ps,
            input_layout,
            blend_state,
            rasterizer_state,
            depth_stencil_state,
            cb_params,
            vb: None,
            vb_capacity: 0,
            ib: None,
            ib_capacity: 0,
            textures: HashMap::new(),
            samplers,
            vertices_scratch: Vec::new(),
            indices_scratch: Vec::new(),
        })
    }

    pub fn update_textures(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        delta: &egui::TexturesDelta,
    ) -> Result<()> {
        for (id, deltas) in &delta.set {
            for image_delta in deltas {
                let egui::ImageData::Color(image) = &image_delta.image;
                let [w, h] = image.size;
                if w == 0 || h == 0 {
                    continue;
                }
                let expected_pixels = w.checked_mul(h).context("texture pixel count overflow")?;
                if image.pixels.len() != expected_pixels {
                    bail!(
                        "texture {id:?} pixel count mismatch: expected {expected_pixels}, got {}",
                        image.pixels.len()
                    );
                }

                let pixels = color_image_to_rgba8(image);
                let width_u32 = u32::try_from(w).context("texture width exceeds u32")?;
                let height_u32 = u32::try_from(h).context("texture height exceeds u32")?;
                let row_pitch = width_u32
                    .checked_mul(4)
                    .context("texture row pitch exceeds u32")?;

                match image_delta.pos {
                    None => {
                        let gpu_tex = create_rgba8_texture(
                            device,
                            width_u32,
                            height_u32,
                            row_pitch,
                            &pixels,
                            image_delta.options,
                        )?;
                        self.textures.insert(*id, gpu_tex);
                    }
                    Some([px, py]) => {
                        let existing = self.textures.get_mut(id).with_context(|| {
                            format!("partial texture update for unknown texture {id:?}")
                        })?;
                        let end_x = px.checked_add(w).context("partial update x overflow")?;
                        let end_y = py.checked_add(h).context("partial update y overflow")?;
                        if end_x > existing.size[0] || end_y > existing.size[1] {
                            bail!(
                                "partial texture update for {id:?} out of bounds: ({px},{py})+{w}x{h} vs {}x{}",
                                existing.size[0],
                                existing.size[1]
                            );
                        }
                        let dst_box = D3D11_BOX {
                            left: u32::try_from(px).context("partial update x exceeds u32")?,
                            top: u32::try_from(py).context("partial update y exceeds u32")?,
                            front: 0,
                            right: u32::try_from(end_x)
                                .context("partial update right exceeds u32")?,
                            bottom: u32::try_from(end_y)
                                .context("partial update bottom exceeds u32")?,
                            back: 1,
                        };
                        unsafe {
                            context.UpdateSubresource(
                                &existing.texture,
                                0,
                                Some(&dst_box),
                                pixels.as_ptr().cast(),
                                row_pitch,
                                0,
                            );
                        }
                        existing.options = image_delta.options;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn paint(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        screen_size_px: (u32, u32),
        pixels_per_point: f32,
        hdr_output: bool,
        primitives: &[egui::ClippedPrimitive],
    ) -> Result<()> {
        if screen_size_px.0 == 0 || screen_size_px.1 == 0 || primitives.is_empty() {
            return Ok(());
        }
        let ppp = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
            pixels_per_point
        } else {
            1.0
        };

        self.vertices_scratch.clear();
        self.indices_scratch.clear();
        let mut draw_cmds = Vec::with_capacity(primitives.len());

        for clipped in primitives {
            let egui::epaint::Primitive::Mesh(mesh) = &clipped.primitive else {
                continue;
            };
            if mesh.indices.is_empty() || mesh.vertices.is_empty() {
                continue;
            }
            let Some(scissor) =
                clip_rect_to_scissor(clipped.clip_rect, ppp, screen_size_px.0, screen_size_px.1)
            else {
                continue;
            };
            let Some((srv, options)) = self
                .textures
                .get(&mesh.texture_id)
                .map(|tex| (tex.srv.clone(), tex.options))
            else {
                tracing::warn!(texture_id = ?mesh.texture_id, "missing egui texture; skipping mesh");
                continue;
            };
            let sampler = get_or_create_sampler(&mut self.samplers, device, options)?;

            let base_vertex =
                i32::try_from(self.vertices_scratch.len()).context("egui vertex count overflow")?;
            let start_index =
                u32::try_from(self.indices_scratch.len()).context("egui index offset overflow")?;
            let index_count =
                u32::try_from(mesh.indices.len()).context("egui mesh index count overflow")?;

            self.vertices_scratch
                .extend(mesh.vertices.iter().map(|v| GpuVertex {
                    pos: [v.pos.x, v.pos.y],
                    uv: [v.uv.x, v.uv.y],
                    color: v.color.to_array(),
                }));
            self.indices_scratch.extend_from_slice(&mesh.indices);

            draw_cmds.push(DrawCmd {
                index_count,
                start_index,
                base_vertex,
                scissor,
                srv,
                sampler,
            });
        }

        if draw_cmds.is_empty() {
            return Ok(());
        }

        self.ensure_vertex_buffer(device, self.vertices_scratch.len())?;
        self.ensure_index_buffer(device, self.indices_scratch.len())?;
        let vb = self.vb.as_ref().context("vertex buffer not initialized")?;
        let ib = self.ib.as_ref().context("index buffer not initialized")?;

        unsafe {
            let mut mapped_vb = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(vb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped_vb))?;
            std::ptr::copy_nonoverlapping(
                self.vertices_scratch.as_ptr(),
                mapped_vb.pData.cast::<GpuVertex>(),
                self.vertices_scratch.len(),
            );
            context.Unmap(vb, 0);

            let mut mapped_ib = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(ib, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped_ib))?;
            std::ptr::copy_nonoverlapping(
                self.indices_scratch.as_ptr(),
                mapped_ib.pData.cast::<u32>(),
                self.indices_scratch.len(),
            );
            context.Unmap(ib, 0);
        }

        let params = ScreenParams {
            screen_size: [screen_size_px.0 as f32 / ppp, screen_size_px.1 as f32 / ppp],
            hdr_paper_white_nits: if hdr_output { 200.0 } else { 0.0 },
            _pad: 0.0,
        };
        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: screen_size_px.0 as f32,
            Height: screen_size_px.1 as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let stride = std::mem::size_of::<GpuVertex>() as u32;
        let offset = 0u32;
        let vb_bind = [Some(vb.clone())];

        // Save rasterizer/blend/depth/scissor state so video rendering on subsequent frames is unaffected.
        let prev_rs = unsafe { context.RSGetState().ok() };
        let mut prev_scissor_count = 16u32;
        let mut prev_scissors = [RECT::default(); 16];
        unsafe {
            context.RSGetScissorRects(
                &raw mut prev_scissor_count,
                Some(prev_scissors.as_mut_ptr()),
            );
        }
        let mut prev_blend = None;
        let mut prev_blend_factor = [1.0f32; 4];
        let mut prev_sample_mask = u32::MAX;
        unsafe {
            context.OMGetBlendState(
                Some(&raw mut prev_blend),
                Some(&mut prev_blend_factor),
                Some(&raw mut prev_sample_mask),
            );
        }
        let mut prev_depth = None;
        let mut prev_stencil_ref = 0u32;
        unsafe {
            context
                .OMGetDepthStencilState(Some(&raw mut prev_depth), Some(&raw mut prev_stencil_ref));
        }

        unsafe {
            context.UpdateSubresource(&self.cb_params, 0, None, (&raw const params).cast(), 0, 0);
            context.RSSetViewports(Some(&[viewport]));
            context.IASetInputLayout(&self.input_layout);
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.IASetVertexBuffers(0, 1, Some(vb_bind.as_ptr()), Some(&stride), Some(&offset));
            context.IASetIndexBuffer(ib, DXGI_FORMAT_R32_UINT, 0);
            let cb = [Some(self.cb_params.clone())];
            context.VSSetShader(&self.vs, None);
            context.VSSetConstantBuffers(0, Some(&cb));
            context.PSSetShader(&self.ps, None);
            context.PSSetConstantBuffers(0, Some(&cb));
            context.RSSetState(&self.rasterizer_state);
            context.OMSetBlendState(&self.blend_state, None, u32::MAX);
            context.OMSetDepthStencilState(&self.depth_stencil_state, 0);

            for cmd in &draw_cmds {
                context.RSSetScissorRects(Some(&[cmd.scissor]));
                context.PSSetShaderResources(0, Some(&[Some(cmd.srv.clone())]));
                context.PSSetSamplers(0, Some(&[Some(cmd.sampler.clone())]));
                context.DrawIndexed(cmd.index_count, cmd.start_index, cmd.base_vertex);
            }

            context.PSSetShaderResources(0, Some(&[None]));
            let saved_count = (prev_scissor_count as usize).min(prev_scissors.len());
            if saved_count == 0 {
                context.RSSetScissorRects(None);
            } else {
                context.RSSetScissorRects(Some(&prev_scissors[..saved_count]));
            }
            context.RSSetState(prev_rs.as_ref());
            context.OMSetBlendState(
                prev_blend.as_ref(),
                Some(&prev_blend_factor),
                prev_sample_mask,
            );
            context.OMSetDepthStencilState(prev_depth.as_ref(), prev_stencil_ref);
        }

        Ok(())
    }

    pub fn free_textures(&mut self, delta: &egui::TexturesDelta) {
        for id in &delta.free {
            self.textures.remove(id);
        }
    }

    fn ensure_vertex_buffer(&mut self, device: &ID3D11Device, required_verts: usize) -> Result<()> {
        if self.vb.is_some() && self.vb_capacity >= required_verts {
            return Ok(());
        }
        let new_cap = required_verts
            .checked_next_power_of_two()
            .unwrap_or(required_verts)
            .max(1024);
        let byte_width = u32::try_from(new_cap * std::mem::size_of::<GpuVertex>())
            .context("vertex buffer byte width exceeds u32")?;
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: byte_width,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            ..Default::default()
        };
        let buf = unsafe {
            let mut out = None;
            device.CreateBuffer(&desc, None, Some(&mut out))?;
            out.context("CreateBuffer(egui VB) returned None")?
        };
        self.vb = Some(buf);
        self.vb_capacity = new_cap;
        Ok(())
    }

    fn ensure_index_buffer(
        &mut self,
        device: &ID3D11Device,
        required_indices: usize,
    ) -> Result<()> {
        if self.ib.is_some() && self.ib_capacity >= required_indices {
            return Ok(());
        }
        let new_cap = required_indices
            .checked_next_power_of_two()
            .unwrap_or(required_indices)
            .max(2048);
        let byte_width = u32::try_from(new_cap * std::mem::size_of::<u32>())
            .context("index buffer byte width exceeds u32")?;
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: byte_width,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_INDEX_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            ..Default::default()
        };
        let buf = unsafe {
            let mut out = None;
            device.CreateBuffer(&desc, None, Some(&mut out))?;
            out.context("CreateBuffer(egui IB) returned None")?
        };
        self.ib = Some(buf);
        self.ib_capacity = new_cap;
        Ok(())
    }
}

pub(crate) fn color_image_to_rgba8(image: &egui::ColorImage) -> Vec<u8> {
    image.pixels.iter().flat_map(|c| c.to_array()).collect()
}

fn create_rgba8_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    row_pitch: u32,
    pixels: &[u8],
    options: egui::TextureOptions,
) -> Result<GpuTexture> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let sub = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr().cast(),
        SysMemPitch: row_pitch,
        SysMemSlicePitch: 0,
    };
    let texture = unsafe {
        let mut out = None;
        device.CreateTexture2D(&desc, Some(&sub), Some(&mut out))?;
        out.context("CreateTexture2D(egui RGBA8) returned None")?
    };
    let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
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
        device.CreateShaderResourceView(&texture, Some(&srv_desc), Some(&mut out))?;
        out.context("CreateShaderResourceView(egui RGBA8) returned None")?
    };

    Ok(GpuTexture {
        texture,
        srv,
        size: [width as usize, height as usize],
        options,
    })
}

fn get_or_create_sampler(
    samplers: &mut HashMap<SamplerKey, ID3D11SamplerState>,
    device: &ID3D11Device,
    options: egui::TextureOptions,
) -> Result<ID3D11SamplerState> {
    let key = SamplerKey::from(options);
    if let Some(existing) = samplers.get(&key) {
        return Ok(existing.clone());
    }
    let sampler = create_sampler(device, key)?;
    samplers.insert(key, sampler.clone());
    Ok(sampler)
}

fn create_sampler(device: &ID3D11Device, key: SamplerKey) -> Result<ID3D11SamplerState> {
    let filter: D3D11_FILTER = match (key.min, key.mag) {
        (egui::TextureFilter::Nearest, egui::TextureFilter::Nearest) => {
            D3D11_FILTER_MIN_MAG_MIP_POINT
        }
        (egui::TextureFilter::Nearest, egui::TextureFilter::Linear) => {
            D3D11_FILTER_MIN_POINT_MAG_LINEAR_MIP_POINT
        }
        (egui::TextureFilter::Linear, egui::TextureFilter::Nearest) => {
            D3D11_FILTER_MIN_LINEAR_MAG_MIP_POINT
        }
        (egui::TextureFilter::Linear, egui::TextureFilter::Linear) => {
            D3D11_FILTER_MIN_MAG_MIP_LINEAR
        }
    };
    let address: D3D11_TEXTURE_ADDRESS_MODE = match key.wrap {
        egui::TextureWrapMode::ClampToEdge => D3D11_TEXTURE_ADDRESS_CLAMP,
        egui::TextureWrapMode::Repeat => D3D11_TEXTURE_ADDRESS_WRAP,
        egui::TextureWrapMode::MirroredRepeat => D3D11_TEXTURE_ADDRESS_MIRROR,
    };
    let desc = D3D11_SAMPLER_DESC {
        Filter: filter,
        AddressU: address,
        AddressV: address,
        AddressW: address,
        ComparisonFunc: D3D11_COMPARISON_NEVER,
        MaxLOD: f32::MAX,
        ..Default::default()
    };
    unsafe {
        let mut out = None;
        device.CreateSamplerState(&desc, Some(&mut out))?;
        out.context("CreateSamplerState(egui) returned None")
    }
}

pub(crate) fn clip_rect_to_scissor(
    clip_rect: egui::Rect,
    pixels_per_point: f32,
    screen_w: u32,
    screen_h: u32,
) -> Option<RECT> {
    if clip_rect.any_nan() || !clip_rect.is_positive() {
        return None;
    }
    let max_w = screen_w as f32;
    let max_h = screen_h as f32;
    let left = (clip_rect.min.x * pixels_per_point)
        .floor()
        .clamp(0.0, max_w) as i32;
    let top = (clip_rect.min.y * pixels_per_point)
        .floor()
        .clamp(0.0, max_h) as i32;
    let right = (clip_rect.max.x * pixels_per_point)
        .ceil()
        .clamp(0.0, max_w) as i32;
    let bottom = (clip_rect.max.y * pixels_per_point)
        .ceil()
        .clamp(0.0, max_h) as i32;

    if right <= left || bottom <= top {
        return None;
    }

    Some(RECT {
        left,
        top,
        right,
        bottom,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_egui_hlsl_shaders() {
        let vs = compile_hlsl(EGUI_VS_HLSL, s!("VSMain"), s!("vs_5_0")).expect("VS compiles");
        let ps = compile_hlsl(EGUI_PS_HLSL, s!("PSMain"), s!("ps_5_0")).expect("PS compiles");

        assert!(!blob_slice(&vs).is_empty());
        assert!(!blob_slice(&ps).is_empty());
    }

    #[test]
    fn gpu_structs_match_d3d11_layout() {
        assert_eq!(std::mem::size_of::<GpuVertex>(), 20);
        assert_eq!(std::mem::align_of::<GpuVertex>(), 4);
        assert_eq!(std::mem::size_of::<ScreenParams>(), 16);
    }

    #[test]
    fn clip_rect_to_scissor_scales_and_clamps() {
        let r = egui::Rect::from_min_max(egui::pos2(-10.0, 10.2), egui::pos2(200.4, 900.0));
        let scissor = clip_rect_to_scissor(r, 1.5, 1920, 1080).expect("valid scissor");
        assert_eq!(
            scissor,
            RECT {
                left: 0,
                top: 15,
                right: 301,
                bottom: 1080,
            }
        );

        let everything = clip_rect_to_scissor(egui::Rect::EVERYTHING, 1.0, 1280, 720)
            .expect("Rect::EVERYTHING clamps to screen");
        assert_eq!(
            everything,
            RECT {
                left: 0,
                top: 0,
                right: 1280,
                bottom: 720,
            }
        );

        let offscreen = egui::Rect::from_min_max(egui::pos2(1300.0, 0.0), egui::pos2(1400.0, 50.0));
        assert!(clip_rect_to_scissor(offscreen, 1.0, 1280, 720).is_none());
        assert!(clip_rect_to_scissor(egui::Rect::NOTHING, 1.0, 1280, 720).is_none());
    }

    #[test]
    fn color_image_to_rgba8_flattens_pixels() {
        let img = egui::ColorImage::new(
            [2, 1],
            vec![
                egui::Color32::from_rgba_premultiplied(10, 20, 30, 40),
                egui::Color32::from_rgba_premultiplied(200, 210, 220, 255),
            ],
        );
        assert_eq!(
            color_image_to_rgba8(&img),
            vec![10, 20, 30, 40, 200, 210, 220, 255]
        );
    }

    #[test]
    fn default_fonts_are_embedded() {
        assert!(!egui::FontDefinitions::default().font_data.is_empty());
    }
}
