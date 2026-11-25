use crate::ffi;
use crate::renderer::{
    ScaleMode, UiMeshDraw, UiOverlayRenderer, UiTextureWrap, VideoFilterMode, VideoRenderOptions,
    VideoRenderer,
};
use crate::video_decoder::DecodedFrame;
use anyhow::{Context, Result, bail};
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_void};

const EGUI_VS_GLSL: &std::ffi::CStr = c"#version 450
layout(location = 0) in vec2 in_pos;
layout(location = 1) in vec2 in_uv;
layout(location = 2) in vec4 in_color;

layout(push_constant, std430) uniform Push {
    vec2 screen_size;
    float hdr_paper_white_nits;
    float flipped;
};

layout(location = 0) out vec2 v_uv;
layout(location = 1) out vec4 v_color;

void main() {
    float x = in_pos.x * 2.0 / screen_size.x - 1.0;
    float y = in_pos.y * 2.0 / screen_size.y - 1.0;
    if (flipped > 0.5) {
        y = -y;
    }
    gl_Position = vec4(x, y, 0.0, 1.0);
    v_uv = in_uv;
    v_color = in_color;
}
";

const EGUI_PS_GLSL: &std::ffi::CStr = c"#version 450
layout(binding = 0) uniform sampler2D tex0;

layout(push_constant, std430) uniform Push {
    vec2 screen_size;
    float hdr_paper_white_nits;
    float flipped;
};

layout(location = 0) in vec2 v_uv;
layout(location = 1) in vec4 v_color;
layout(location = 0) out vec4 out_color;

vec3 srgb_to_linear_egui(vec3 c) {
    vec3 lo = c / 12.92;
    vec3 hi = pow(max((c + 0.055) / 1.055, vec3(0.0)), vec3(2.4));
    return mix(hi, lo, lessThanEqual(c, vec3(0.04045)));
}

vec3 linear_to_pq_egui(vec3 lin) {
    vec3 ym = pow(clamp(lin, 0.0, 1.0), vec3(0.1593017578125));
    return pow((0.8359375 + 18.8515625 * ym) / (1.0 + 18.6875 * ym), vec3(78.84375));
}

void main() {
    vec4 col = v_color * texture(tex0, v_uv);
    if (hdr_paper_white_nits > 0.5) {
        float a = col.a;
        vec3 srgb = (a > 1e-4) ? clamp(col.rgb / a, 0.0, 1.0) : vec3(0.0);
        vec3 lin709 = srgb_to_linear_egui(srgb);
        vec3 lin2020 = vec3(
            0.627404 * lin709.r + 0.329283 * lin709.g + 0.043313 * lin709.b,
            0.069097 * lin709.r + 0.919540 * lin709.g + 0.011363 * lin709.b,
            0.016391 * lin709.r + 0.088013 * lin709.g + 0.895596 * lin709.b
        );
        vec3 pq = linear_to_pq_egui(lin2020 * (hdr_paper_white_nits / 10000.0));
        out_color = vec4(pq * a, a);
        return;
    }
    out_color = col;
}
";

#[repr(C)]
#[derive(Clone, Copy)]
struct UiPushConstants {
    screen_size: [f32; 2],
    hdr_paper_white_nits: f32,
    flipped: f32,
}

fn map_ui_wrap_mode(wrap: UiTextureWrap) -> ffi::pl_tex_address_mode {
    match wrap {
        UiTextureWrap::ClampToEdge => ffi::pl_tex_address_mode_PL_TEX_ADDRESS_CLAMP,
        UiTextureWrap::Repeat => ffi::pl_tex_address_mode_PL_TEX_ADDRESS_REPEAT,
        UiTextureWrap::MirroredRepeat => ffi::pl_tex_address_mode_PL_TEX_ADDRESS_MIRROR,
    }
}

const CAS_HOOK_GLSL: &str = "\
//!PARAM sharpness
//!TYPE DYNAMIC float
//!MINIMUM 0.0
//!MAXIMUM 1.0
0.0

//!HOOK OUTPUT
//!BIND HOOKED
//!DESC gnsh-play CAS
vec4 hook() {
    vec4 c0 = HOOKED_tex(HOOKED_pos);
    vec3 cas_c = c0.rgb;
    vec3 cas_n = HOOKED_tex(HOOKED_pos + vec2(0.0, -HOOKED_pt.y)).rgb;
    vec3 cas_s = HOOKED_tex(HOOKED_pos + vec2(0.0,  HOOKED_pt.y)).rgb;
    vec3 cas_w = HOOKED_tex(HOOKED_pos + vec2(-HOOKED_pt.x, 0.0)).rgb;
    vec3 cas_e = HOOKED_tex(HOOKED_pos + vec2( HOOKED_pt.x, 0.0)).rgb;
    vec3 mn = min(cas_c, min(min(cas_n, cas_s), min(cas_w, cas_e)));
    vec3 mx = max(cas_c, max(max(cas_n, cas_s), max(cas_w, cas_e)));
    vec3 amp = sqrt(clamp(min(mn, 1.0 - mx) / max(mx, vec3(1e-4)), 0.0, 1.0));
    float peak = -1.0 / mix(8.0, 5.0, clamp(sharpness, 0.0, 1.0));
    vec3 weight = amp * peak;
    vec3 sharp = clamp((cas_c + (cas_n + cas_s + cas_w + cas_e) * weight) / (1.0 + 4.0 * weight), 0.0, 1.0);
    return vec4(mix(cas_c, sharp, clamp(sharpness, 0.0, 1.0)), c0.a);
}
";

pub struct VulkanPlaceboRenderer {
    log: ffi::pl_log,
    vk_inst: ffi::pl_vk_inst,
    surface: *mut c_void,
    destroy_surface: unsafe fn(*mut c_void, *mut c_void),
    vk: ffi::pl_vulkan,
    sw: ffi::pl_swapchain,
    renderer: ffi::pl_renderer,
    cas_hook: *const ffi::pl_hook,
    rgba8_fmt: ffi::pl_fmt,
    vert_pos_fmt: ffi::pl_fmt,
    vert_color_fmt: ffi::pl_fmt,
    video_planes: [ffi::pl_tex; 3],
    ui_textures: HashMap<u64, ffi::pl_tex>,
    ui_passes: HashMap<u64, ffi::pl_pass>,
    active_frame: Cell<Option<ffi::pl_swapchain_frame>>,
    pending_swap_buffers: Cell<bool>,
    hdr_output_active: Cell<bool>,
    hdr_requested: bool,
    vsync: bool,
    width: u32,
    height: u32,
}

impl VulkanPlaceboRenderer {
    /// Creates a `libplacebo`-backed Vulkan swapchain and renderer for an SDL3 Vulkan window.
    ///
    /// # Safety
    ///
    /// `required_extensions` pointers must remain valid C strings for the duration of this call,
    /// and `destroy_surface` must match the `VkInstance` / `VkSurfaceKHR` handles created by
    /// `surface_factory`.
    pub unsafe fn new(
        required_extensions: &[*const c_char],
        surface_factory: impl FnOnce(*mut c_void) -> Result<*mut c_void>,
        destroy_surface: unsafe fn(*mut c_void, *mut c_void),
        width: u32,
        height: u32,
        vsync: bool,
    ) -> Result<Self> {
        let log_params = ffi::pl_log_params {
            log_cb: Some(placebo_log_cb),
            log_priv: std::ptr::null_mut(),
            log_level: ffi::pl_log_level_PL_LOG_WARN,
        };
        let log = unsafe { ffi::pl_log_create_387(ffi::PL_API_VER as c_int, &log_params) };
        if log.is_null() {
            bail!("pl_log_create returned null");
        }

        let mut vk_inst_params = unsafe { ffi::pl_vk_inst_default_params };
        vk_inst_params.extensions = required_extensions.as_ptr();
        vk_inst_params.num_extensions = i32::try_from(required_extensions.len())
            .context("Vulkan instance extension count exceeds i32")?;
        let mut vk_inst = unsafe { ffi::pl_vk_inst_create(log, &vk_inst_params) };
        if vk_inst.is_null() {
            let mut log_mut = log;
            unsafe { ffi::pl_log_destroy(&mut log_mut) };
            bail!("pl_vk_inst_create failed (missing Vulkan driver or surface extension)");
        }

        let instance_ptr = unsafe { (*vk_inst).instance.cast::<c_void>() };
        let surface = match surface_factory(instance_ptr) {
            Ok(s) if !s.is_null() => s,
            Ok(_) => {
                let mut log_mut = log;
                unsafe {
                    ffi::pl_vk_inst_destroy(&mut vk_inst);
                    ffi::pl_log_destroy(&mut log_mut);
                }
                bail!("Vulkan surface factory returned null surface");
            }
            Err(err) => {
                let mut log_mut = log;
                unsafe {
                    ffi::pl_vk_inst_destroy(&mut vk_inst);
                    ffi::pl_log_destroy(&mut log_mut);
                }
                return Err(err);
            }
        };

        let mut vk_params = unsafe { ffi::pl_vulkan_default_params };
        unsafe {
            vk_params.instance = (*vk_inst).instance;
            vk_params.get_proc_addr = (*vk_inst).get_proc_addr;
            vk_params.surface = surface.cast();
            // Execute uploads and rendering on a single queue to avoid cross-queue semaphore stalls on AMD.
            vk_params.async_transfer = false;
        }
        let mut vk = unsafe { ffi::pl_vulkan_create(log, &vk_params) };
        if vk.is_null() {
            let mut log_mut = log;
            unsafe {
                destroy_surface(instance_ptr, surface);
                ffi::pl_vk_inst_destroy(&mut vk_inst);
                ffi::pl_log_destroy(&mut log_mut);
            }
            bail!("pl_vulkan_create failed to find a Vulkan device supporting presentation");
        }

        let gpu = unsafe { (*vk).gpu };
        let sw_params = build_swapchain_params(surface.cast(), vsync);
        let mut sw = unsafe { ffi::pl_vulkan_create_swapchain(vk, &sw_params) };
        if sw.is_null() {
            let mut log_mut = log;
            unsafe {
                ffi::pl_gpu_finish(gpu);
                ffi::pl_vulkan_destroy(&mut vk);
                destroy_surface(instance_ptr, surface);
                ffi::pl_vk_inst_destroy(&mut vk_inst);
                ffi::pl_log_destroy(&mut log_mut);
            }
            bail!("pl_vulkan_create_swapchain failed");
        }
        tracing::info!(
            vsync,
            swapchain_latency = unsafe { ffi::pl_swapchain_latency(sw) },
            "Vulkan swapchain configured"
        );

        let mut w = width.max(1) as c_int;
        let mut h = height.max(1) as c_int;
        let resize_ok = unsafe { ffi::pl_swapchain_resize(sw, &mut w, &mut h) };
        if !resize_ok {
            let mut log_mut = log;
            unsafe {
                ffi::pl_gpu_finish(gpu);
                ffi::pl_swapchain_destroy(&mut sw);
                ffi::pl_vulkan_destroy(&mut vk);
                destroy_surface(instance_ptr, surface);
                ffi::pl_vk_inst_destroy(&mut vk_inst);
                ffi::pl_log_destroy(&mut log_mut);
            }
            bail!("initial pl_swapchain_resize failed");
        }

        let mut renderer = unsafe { ffi::pl_renderer_create(log, gpu) };
        if renderer.is_null() {
            let mut log_mut = log;
            unsafe {
                ffi::pl_gpu_finish(gpu);
                ffi::pl_swapchain_destroy(&mut sw);
                ffi::pl_vulkan_destroy(&mut vk);
                destroy_surface(instance_ptr, surface);
                ffi::pl_vk_inst_destroy(&mut vk_inst);
                ffi::pl_log_destroy(&mut log_mut);
            }
            bail!("pl_renderer_create failed");
        }

        let rgba8_fmt = unsafe {
            ffi::pl_find_fmt(
                gpu,
                ffi::pl_fmt_type_PL_FMT_UNORM,
                4,
                8,
                8,
                (ffi::pl_fmt_caps_PL_FMT_CAP_SAMPLEABLE | ffi::pl_fmt_caps_PL_FMT_CAP_LINEAR)
                    as ffi::pl_fmt_caps,
            )
        };
        let vert_pos_fmt =
            unsafe { ffi::pl_find_vertex_fmt(gpu, ffi::pl_fmt_type_PL_FMT_FLOAT, 2) };
        let vert_color_fmt = unsafe {
            ffi::pl_find_fmt(
                gpu,
                ffi::pl_fmt_type_PL_FMT_UNORM,
                4,
                8,
                8,
                ffi::pl_fmt_caps_PL_FMT_CAP_VERTEX,
            )
        };
        if rgba8_fmt.is_null() || vert_pos_fmt.is_null() || vert_color_fmt.is_null() {
            let mut log_mut = log;
            unsafe {
                ffi::pl_gpu_finish(gpu);
                ffi::pl_renderer_destroy(&mut renderer);
                ffi::pl_swapchain_destroy(&mut sw);
                ffi::pl_vulkan_destroy(&mut vk);
                destroy_surface(instance_ptr, surface);
                ffi::pl_vk_inst_destroy(&mut vk_inst);
                ffi::pl_log_destroy(&mut log_mut);
            }
            bail!("required libplacebo Vulkan formats are unavailable");
        }

        let cas_hook = unsafe {
            ffi::pl_mpv_user_shader_parse(gpu, CAS_HOOK_GLSL.as_ptr().cast(), CAS_HOOK_GLSL.len())
        };
        if cas_hook.is_null() {
            tracing::warn!(
                "libplacebo pl_mpv_user_shader_parse(CAS) returned null; sharpening disabled"
            );
        }

        Ok(Self {
            log,
            vk_inst,
            surface,
            destroy_surface,
            vk,
            sw,
            renderer,
            cas_hook,
            rgba8_fmt,
            vert_pos_fmt,
            vert_color_fmt,
            video_planes: [std::ptr::null(); 3],
            ui_textures: HashMap::new(),
            ui_passes: HashMap::new(),
            active_frame: Cell::new(None),
            pending_swap_buffers: Cell::new(false),
            hdr_output_active: Cell::new(false),
            hdr_requested: false,
            vsync,
            width: u32::try_from(w.max(1)).unwrap_or(1),
            height: u32::try_from(h.max(1)).unwrap_or(1),
        })
    }

    fn flush_swap_buffers(&self) {
        if self.pending_swap_buffers.replace(false) && !self.sw.is_null() {
            unsafe {
                ffi::pl_swapchain_swap_buffers(self.sw);
            }
        }
    }

    pub fn set_vsync(&mut self, vsync: bool) -> Result<()> {
        if self.vsync == vsync {
            return Ok(());
        }
        if self.active_frame.get().is_some() {
            bail!("cannot recreate Vulkan swapchain while a frame is active");
        }

        self.flush_swap_buffers();
        let gpu = unsafe { (*self.vk).gpu };
        unsafe {
            ffi::pl_gpu_finish(gpu);
        }
        for (_, mut pass) in self.ui_passes.drain() {
            unsafe {
                ffi::pl_pass_destroy(gpu, &mut pass);
            }
        }
        unsafe {
            ffi::pl_swapchain_destroy(&mut self.sw);
        }

        let sw_params = build_swapchain_params(self.surface.cast(), vsync);
        let sw = unsafe { ffi::pl_vulkan_create_swapchain(self.vk, &sw_params) };
        if sw.is_null() {
            bail!("pl_vulkan_create_swapchain failed while updating VSync");
        }
        tracing::info!(
            vsync,
            swapchain_latency = unsafe { ffi::pl_swapchain_latency(sw) },
            "Vulkan swapchain configured"
        );
        self.sw = sw;
        self.vsync = vsync;

        if self.hdr_requested {
            unsafe {
                ffi::pl_swapchain_colorspace_hint(self.sw, &raw const ffi::pl_color_space_hdr10);
            }
        }

        let mut w = self.width.max(1) as c_int;
        let mut h = self.height.max(1) as c_int;
        if !unsafe { ffi::pl_swapchain_resize(self.sw, &mut w, &mut h) } {
            bail!("pl_swapchain_resize failed after VSync swapchain recreation");
        }
        self.width = u32::try_from(w.max(1)).unwrap_or(self.width);
        self.height = u32::try_from(h.max(1)).unwrap_or(self.height);
        Ok(())
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
                "UI texture upload slice too small: need {expected}, got {}",
                rgba.len()
            );
        }

        let gpu = unsafe { (*self.vk).gpu };
        let mut tex = self.ui_textures.remove(&id).unwrap_or(std::ptr::null());
        let mut params: ffi::pl_tex_params = unsafe { std::mem::zeroed() };
        params.w = i32::try_from(w).context("UI texture width exceeds i32")?;
        params.h = i32::try_from(h).context("UI texture height exceeds i32")?;
        params.format = self.rgba8_fmt;
        params.sampleable = true;
        params.host_writable = true;
        params.debug_tag = c"egui".as_ptr();

        let ok = unsafe { ffi::pl_tex_recreate(gpu, &mut tex, &params) };
        if !ok || tex.is_null() {
            bail!("pl_tex_recreate(egui RGBA8) failed");
        }
        self.ui_textures.insert(id, tex);
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
            .context("UI texture update size overflow")?;
        if rgba.len() < expected {
            bail!(
                "UI texture region slice too small: need {expected}, got {}",
                rgba.len()
            );
        }

        let tex = *self
            .ui_textures
            .get(&id)
            .with_context(|| format!("missing Vulkan UI texture id {id}"))?;
        let (tex_w, tex_h) = unsafe { ((*tex).params.w, (*tex).params.h) };
        let end_x = i32::try_from(x.saturating_add(w)).context("UI texture x1 exceeds i32")?;
        let end_y = i32::try_from(y.saturating_add(h)).context("UI texture y1 exceeds i32")?;
        if end_x > tex_w || end_y > tex_h {
            bail!("UI texture region out of bounds: ({x},{y})+{w}x{h} vs {tex_w}x{tex_h}");
        }

        let mut xfer: ffi::pl_tex_transfer_params = unsafe { std::mem::zeroed() };
        xfer.tex = tex;
        xfer.rc = ffi::pl_rect3d {
            x0: i32::try_from(x).context("UI texture x0 exceeds i32")?,
            y0: i32::try_from(y).context("UI texture y0 exceeds i32")?,
            z0: 0,
            x1: end_x,
            y1: end_y,
            z1: 1,
        };
        xfer.row_pitch = (w as usize) * 4;
        xfer.ptr = rgba.as_ptr().cast_mut().cast();

        let gpu = unsafe { (*self.vk).gpu };
        if !unsafe { ffi::pl_tex_upload(gpu, &xfer) } {
            bail!("pl_tex_upload(egui RGBA8) failed");
        }
        Ok(())
    }

    pub fn free_ui_texture(&mut self, id: u64) {
        if let Some(mut tex) = self.ui_textures.remove(&id) {
            let gpu = unsafe { (*self.vk).gpu };
            unsafe {
                ffi::pl_tex_destroy(gpu, &mut tex);
            }
        }
    }

    pub fn draw_ui_mesh(&mut self, draw: UiMeshDraw<'_>) -> Result<()> {
        let Some(sw_frame) = self.active_frame.get() else {
            return Ok(());
        };
        if draw.vertices_bytes.is_empty() || draw.indices.is_empty() {
            return Ok(());
        }
        let Some(&tex) = self.ui_textures.get(&draw.tex_id) else {
            tracing::warn!(
                tex_id = draw.tex_id,
                "missing Vulkan UI texture; skipping mesh"
            );
            return Ok(());
        };

        let target_tex = sw_frame.fbo;
        if target_tex.is_null() {
            return Ok(());
        }
        let (target_w, target_h, target_fmt) = unsafe {
            (
                (*target_tex).params.w,
                (*target_tex).params.h,
                (*target_tex).params.format,
            )
        };
        if target_fmt.is_null() || target_w <= 0 || target_h <= 0 {
            return Ok(());
        }
        let fmt_sig = unsafe { (*target_fmt).signature };

        if !self.ui_passes.contains_key(&fmt_sig) {
            let pass = self.create_ui_pass(target_fmt)?;
            self.ui_passes.insert(fmt_sig, pass);
        }
        let pass = self.ui_passes[&fmt_sig];

        let mut push = UiPushConstants {
            screen_size: draw.screen_size_pts,
            hdr_paper_white_nits: draw.hdr_paper_white_nits,
            flipped: if sw_frame.flipped { 1.0 } else { 0.0 },
        };
        let mut binding = ffi::pl_desc_binding {
            object: tex.cast(),
            address_mode: map_ui_wrap_mode(draw.wrap),
            sample_mode: if draw.linear_filter {
                ffi::pl_tex_sample_mode_PL_TEX_SAMPLE_LINEAR
            } else {
                ffi::pl_tex_sample_mode_PL_TEX_SAMPLE_NEAREST
            },
        };

        let [left, top, right, bottom] = draw.scissor;
        let scissors = if sw_frame.flipped {
            ffi::pl_rect2d {
                x0: left.clamp(0, target_w),
                y0: (target_h - bottom).clamp(0, target_h),
                x1: right.clamp(0, target_w),
                y1: (target_h - top).clamp(0, target_h),
            }
        } else {
            ffi::pl_rect2d {
                x0: left.clamp(0, target_w),
                y0: top.clamp(0, target_h),
                x1: right.clamp(0, target_w),
                y1: bottom.clamp(0, target_h),
            }
        };
        if scissors.x1 <= scissors.x0 || scissors.y1 <= scissors.y0 {
            return Ok(());
        }

        let mut run: ffi::pl_pass_run_params = unsafe { std::mem::zeroed() };
        run.pass = pass;
        run.desc_bindings = &raw mut binding;
        run.push_constants = (&raw mut push).cast();
        run.target = target_tex;
        run.viewport = ffi::pl_rect2d {
            x0: 0,
            y0: 0,
            x1: target_w,
            y1: target_h,
        };
        run.scissors = scissors;
        run.vertex_count =
            i32::try_from(draw.indices.len()).context("UI index count exceeds i32")?;
        run.vertex_data = draw.vertices_bytes.as_ptr().cast();
        run.index_data = draw.indices.as_ptr().cast();
        run.index_fmt = ffi::pl_index_format_PL_INDEX_UINT32;

        let gpu = unsafe { (*self.vk).gpu };
        unsafe {
            ffi::pl_pass_run(gpu, &run);
        }
        Ok(())
    }

    fn create_ui_pass(&self, target_format: ffi::pl_fmt) -> Result<ffi::pl_pass> {
        let mut descs = [ffi::pl_desc {
            name: c"tex0".as_ptr(),
            type_: ffi::pl_desc_type_PL_DESC_SAMPLED_TEX,
            binding: 0,
            access: ffi::pl_desc_access_PL_DESC_ACCESS_READONLY,
        }];
        let mut attribs = [
            ffi::pl_vertex_attrib {
                name: c"in_pos".as_ptr(),
                fmt: self.vert_pos_fmt,
                offset: 0,
                location: 0,
            },
            ffi::pl_vertex_attrib {
                name: c"in_uv".as_ptr(),
                fmt: self.vert_pos_fmt,
                offset: 8,
                location: 1,
            },
            ffi::pl_vertex_attrib {
                name: c"in_color".as_ptr(),
                fmt: self.vert_color_fmt,
                offset: 16,
                location: 2,
            },
        ];
        let mut params: ffi::pl_pass_params = unsafe { std::mem::zeroed() };
        params.type_ = ffi::pl_pass_type_PL_PASS_RASTER;
        params.descriptors = descs.as_mut_ptr();
        params.num_descriptors = descs.len() as c_int;
        params.push_constants_size = std::mem::size_of::<UiPushConstants>();
        params.glsl_shader = EGUI_PS_GLSL.as_ptr();
        params.vertex_type = ffi::pl_prim_type_PL_PRIM_TRIANGLE_LIST;
        params.vertex_attribs = attribs.as_mut_ptr();
        params.num_vertex_attribs = attribs.len() as c_int;
        params.vertex_stride = 20;
        params.vertex_shader = EGUI_VS_GLSL.as_ptr();
        params.target_format = target_format;
        params.blend_params = &raw const ffi::pl_alpha_overlay;
        params.load_target = true;

        let gpu = unsafe { (*self.vk).gpu };
        let pass = unsafe { ffi::pl_pass_create(gpu, &params) };
        if pass.is_null() {
            bail!("pl_pass_create(egui overlay) failed");
        }
        Ok(pass)
    }
}

impl VideoRenderer for VulkanPlaceboRenderer {
    fn viewport_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.flush_swap_buffers();
        let mut w = width.max(1) as c_int;
        let mut h = height.max(1) as c_int;
        if !unsafe { ffi::pl_swapchain_resize(self.sw, &mut w, &mut h) } {
            bail!("pl_swapchain_resize({width}x{height}) failed");
        }
        self.width = u32::try_from(w.max(1)).unwrap_or(self.width);
        self.height = u32::try_from(h.max(1)).unwrap_or(self.height);
        Ok(())
    }

    fn is_display_hdr(&self) -> bool {
        self.hdr_output_active.get() || self.hdr_requested
    }

    fn is_hdr_output_active(&self) -> bool {
        self.hdr_output_active.get()
    }

    fn set_hdr_output(&mut self, enable: bool) -> Result<bool> {
        self.hdr_requested = enable;
        let hint = if enable {
            &raw const ffi::pl_color_space_hdr10
        } else {
            std::ptr::null()
        };
        unsafe {
            ffi::pl_swapchain_colorspace_hint(self.sw, hint);
        }
        Ok(self.hdr_output_active.get())
    }

    fn wait_for_frame_latency(&self, _timeout_ms: u32) {
        self.flush_swap_buffers();
    }

    fn clear_video_cache(&mut self) {
        self.flush_swap_buffers();
        let gpu = unsafe { (*self.vk).gpu };
        unsafe {
            ffi::pl_gpu_finish(gpu);
        }
        for slot in &mut self.video_planes {
            if !slot.is_null() {
                unsafe {
                    ffi::pl_tex_destroy(gpu, slot);
                }
                *slot = std::ptr::null();
            }
        }
    }

    fn begin_frame(&self, clear_rgba: [f32; 4]) {
        if self.active_frame.get().is_some() {
            return;
        }
        self.flush_swap_buffers();
        let mut sw_frame: ffi::pl_swapchain_frame = unsafe { std::mem::zeroed() };
        if !unsafe { ffi::pl_swapchain_start_frame(self.sw, &mut sw_frame) } {
            self.active_frame.set(None);
            return;
        }
        let hdr_active = unsafe { ffi::pl_color_space_is_hdr(&sw_frame.color_space) };
        self.hdr_output_active.set(hdr_active);

        let gpu = unsafe { (*self.vk).gpu };
        let clear = ffi::pl_clear_color { f: clear_rgba };
        unsafe {
            ffi::pl_tex_clear_ex(gpu, sw_frame.fbo, clear);
        }
        self.active_frame.set(Some(sw_frame));
    }

    fn draw_video_frame(
        &mut self,
        frame: &DecodedFrame,
        options: VideoRenderOptions,
    ) -> Result<()> {
        let Some(sw_frame) = self.active_frame.get() else {
            return Ok(());
        };

        let gpu = unsafe { (*self.vk).gpu };
        let mut src_frame: ffi::pl_frame = unsafe { std::mem::zeroed() };
        let (fw, fh, is_hdr, used_planes) = match frame {
            DecodedFrame::D3D11(_) | DecodedFrame::D3D12(_) => {
                bail!("VulkanPlaceboRenderer cannot consume zero-copy D3D11/D3D12 frames directly");
            }
            DecodedFrame::SoftwareNv12 {
                y,
                uv,
                y_stride,
                uv_stride,
                width,
                height,
                is_hdr,
                ..
            } => {
                let cw = width.div_ceil(2) as c_int;
                let ch = height.div_ceil(2) as c_int;
                let y_desc = ffi::pl_plane_data {
                    type_: ffi::pl_fmt_type_PL_FMT_UNORM,
                    width: *width as c_int,
                    height: *height as c_int,
                    component_size: [8, 0, 0, 0],
                    component_pad: [0; 4],
                    component_map: [0, -1, -1, -1],
                    pixel_stride: 1,
                    row_stride: *y_stride,
                    swapped: false,
                    pixels: y.as_ptr().cast(),
                    buf: std::ptr::null(),
                    buf_offset: 0,
                    callback: None,
                    priv_: std::ptr::null_mut(),
                };
                let uv_desc = ffi::pl_plane_data {
                    type_: ffi::pl_fmt_type_PL_FMT_UNORM,
                    width: cw,
                    height: ch,
                    component_size: [8, 8, 0, 0],
                    component_pad: [0; 4],
                    component_map: [1, 2, -1, -1],
                    pixel_stride: 2,
                    row_stride: *uv_stride,
                    swapped: false,
                    pixels: uv.as_ptr().cast(),
                    buf: std::ptr::null(),
                    buf_offset: 0,
                    callback: None,
                    priv_: std::ptr::null_mut(),
                };
                if !unsafe {
                    ffi::pl_upload_plane(
                        gpu,
                        &mut src_frame.planes[0],
                        &mut self.video_planes[0],
                        &y_desc,
                    )
                } || !unsafe {
                    ffi::pl_upload_plane(
                        gpu,
                        &mut src_frame.planes[1],
                        &mut self.video_planes[1],
                        &uv_desc,
                    )
                } {
                    bail!("pl_upload_plane(NV12) failed");
                }
                src_frame.repr = if *is_hdr {
                    unsafe { ffi::pl_color_repr_uhdtv }
                } else {
                    unsafe { ffi::pl_color_repr_hdtv }
                };
                (*width, *height, *is_hdr, 2)
            }
            DecodedFrame::SoftwareP010 {
                y,
                uv,
                y_stride,
                uv_stride,
                width,
                height,
                is_hdr,
                ..
            } => {
                let cw = width.div_ceil(2) as c_int;
                let ch = height.div_ceil(2) as c_int;
                let y_desc = ffi::pl_plane_data {
                    type_: ffi::pl_fmt_type_PL_FMT_UNORM,
                    width: *width as c_int,
                    height: *height as c_int,
                    component_size: [16, 0, 0, 0],
                    component_pad: [0; 4],
                    component_map: [0, -1, -1, -1],
                    pixel_stride: 2,
                    row_stride: *y_stride,
                    swapped: false,
                    pixels: y.as_ptr().cast(),
                    buf: std::ptr::null(),
                    buf_offset: 0,
                    callback: None,
                    priv_: std::ptr::null_mut(),
                };
                let uv_desc = ffi::pl_plane_data {
                    type_: ffi::pl_fmt_type_PL_FMT_UNORM,
                    width: cw,
                    height: ch,
                    component_size: [16, 16, 0, 0],
                    component_pad: [0; 4],
                    component_map: [1, 2, -1, -1],
                    pixel_stride: 4,
                    row_stride: *uv_stride,
                    swapped: false,
                    pixels: uv.as_ptr().cast(),
                    buf: std::ptr::null(),
                    buf_offset: 0,
                    callback: None,
                    priv_: std::ptr::null_mut(),
                };
                if !unsafe {
                    ffi::pl_upload_plane(
                        gpu,
                        &mut src_frame.planes[0],
                        &mut self.video_planes[0],
                        &y_desc,
                    )
                } || !unsafe {
                    ffi::pl_upload_plane(
                        gpu,
                        &mut src_frame.planes[1],
                        &mut self.video_planes[1],
                        &uv_desc,
                    )
                } {
                    bail!("pl_upload_plane(P010) failed");
                }
                let mut repr = unsafe { ffi::pl_color_repr_uhdtv };
                repr.bits = ffi::pl_bit_encoding {
                    sample_depth: 16,
                    color_depth: 10,
                    bit_shift: 6,
                };
                src_frame.repr = repr;
                (*width, *height, *is_hdr, 2)
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
                is_hdr,
                ..
            } => {
                let cw = width.div_ceil(2) as c_int;
                let ch = height.div_ceil(2) as c_int;
                let planes_data = [
                    ffi::pl_plane_data {
                        type_: ffi::pl_fmt_type_PL_FMT_UNORM,
                        width: *width as c_int,
                        height: *height as c_int,
                        component_size: [8, 0, 0, 0],
                        component_pad: [0; 4],
                        component_map: [0, -1, -1, -1],
                        pixel_stride: 1,
                        row_stride: *y_stride,
                        swapped: false,
                        pixels: y.as_ptr().cast(),
                        buf: std::ptr::null(),
                        buf_offset: 0,
                        callback: None,
                        priv_: std::ptr::null_mut(),
                    },
                    ffi::pl_plane_data {
                        type_: ffi::pl_fmt_type_PL_FMT_UNORM,
                        width: cw,
                        height: ch,
                        component_size: [8, 0, 0, 0],
                        component_pad: [0; 4],
                        component_map: [1, -1, -1, -1],
                        pixel_stride: 1,
                        row_stride: *u_stride,
                        swapped: false,
                        pixels: u.as_ptr().cast(),
                        buf: std::ptr::null(),
                        buf_offset: 0,
                        callback: None,
                        priv_: std::ptr::null_mut(),
                    },
                    ffi::pl_plane_data {
                        type_: ffi::pl_fmt_type_PL_FMT_UNORM,
                        width: cw,
                        height: ch,
                        component_size: [8, 0, 0, 0],
                        component_pad: [0; 4],
                        component_map: [2, -1, -1, -1],
                        pixel_stride: 1,
                        row_stride: *v_stride,
                        swapped: false,
                        pixels: v.as_ptr().cast(),
                        buf: std::ptr::null(),
                        buf_offset: 0,
                        callback: None,
                        priv_: std::ptr::null_mut(),
                    },
                ];
                for (i, desc) in planes_data.iter().enumerate() {
                    if !unsafe {
                        ffi::pl_upload_plane(
                            gpu,
                            &mut src_frame.planes[i],
                            &mut self.video_planes[i],
                            desc,
                        )
                    } {
                        bail!("pl_upload_plane(YUV420 plane {i}) failed");
                    }
                }
                src_frame.repr = if *is_hdr {
                    unsafe { ffi::pl_color_repr_uhdtv }
                } else {
                    unsafe { ffi::pl_color_repr_hdtv }
                };
                (*width, *height, *is_hdr, 3)
            }
        };

        for slot in &mut self.video_planes[used_planes..] {
            if !slot.is_null() {
                unsafe {
                    ffi::pl_tex_destroy(gpu, slot);
                }
                *slot = std::ptr::null();
            }
        }

        src_frame.num_planes = used_planes as c_int;
        src_frame.color = if is_hdr {
            unsafe { ffi::pl_color_space_hdr10 }
        } else {
            unsafe { ffi::pl_color_space_srgb }
        };
        src_frame.crop = ffi::pl_rect2df {
            x0: 0.0,
            y0: 0.0,
            x1: fw as f32,
            y1: fh as f32,
        };
        unsafe {
            ffi::pl_frame_set_chroma_location(
                &mut src_frame,
                ffi::pl_chroma_location_PL_CHROMA_LEFT,
            );
        }

        let mut target: ffi::pl_frame = unsafe { std::mem::zeroed() };
        unsafe {
            ffi::pl_frame_from_swapchain(&mut target, &sw_frame);
        }
        let target_w = if sw_frame.fbo.is_null() {
            self.width
        } else {
            unsafe { u32::try_from((*sw_frame.fbo).params.w.max(1)).unwrap_or(self.width) }
        };
        let target_h = if sw_frame.fbo.is_null() {
            self.height
        } else {
            unsafe { u32::try_from((*sw_frame.fbo).params.h.max(1)).unwrap_or(self.height) }
        };
        let [x0, y0, x1, y1] =
            compute_placebo_target_rect(fw, fh, target_w, target_h, options.scale_mode);
        target.crop = ffi::pl_rect2df { x0, y0, x1, y1 };

        let mut params = unsafe { ffi::pl_render_fast_params };
        params.skip_caching_single_frame = true;
        params.disable_linear_scaling = true;
        params.upscaler = match options.filter_mode {
            VideoFilterMode::Bilinear => &raw const ffi::pl_filter_bilinear,
            VideoFilterMode::Cas | VideoFilterMode::Fsr1 => &raw const ffi::pl_filter_spline36,
        };
        let hook_ptrs = [self.cas_hook];
        let sharp = options.sharpness.clamp(0.0, 1.0);
        if !matches!(options.filter_mode, VideoFilterMode::Bilinear)
            && sharp > 0.001
            && !self.cas_hook.is_null()
        {
            unsafe {
                let hook_ref = &*self.cas_hook;
                if hook_ref.num_parameters > 0
                    && !hook_ref.parameters.is_null()
                    && !(*hook_ref.parameters).data.is_null()
                {
                    (*(*hook_ref.parameters).data).f = sharp;
                }
            }
            params.hooks = hook_ptrs.as_ptr();
            params.num_hooks = 1;
        }
        params.border = ffi::pl_clear_mode_PL_CLEAR_SKIP;

        if !unsafe { ffi::pl_render_image(self.renderer, &src_frame, &target, &params) } {
            bail!("pl_render_image failed");
        }
        Ok(())
    }

    fn present(&self, _vsync: bool) -> Result<()> {
        if self.active_frame.take().is_none() {
            return Ok(());
        }
        if !unsafe { ffi::pl_swapchain_submit_frame(self.sw) } {
            bail!("pl_swapchain_submit_frame failed");
        }
        self.pending_swap_buffers.set(true);
        Ok(())
    }
}

impl UiOverlayRenderer for VulkanPlaceboRenderer {
    fn upsert_ui_texture(&mut self, id: u64, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
        VulkanPlaceboRenderer::upsert_ui_texture(self, id, w, h, rgba)
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
        VulkanPlaceboRenderer::update_ui_texture_region(self, id, x, y, w, h, rgba)
    }

    fn free_ui_texture(&mut self, id: u64) {
        VulkanPlaceboRenderer::free_ui_texture(self, id);
    }

    fn draw_ui_mesh(&mut self, draw: UiMeshDraw<'_>) -> Result<()> {
        VulkanPlaceboRenderer::draw_ui_mesh(self, draw)
    }
}

impl Drop for VulkanPlaceboRenderer {
    fn drop(&mut self) {
        self.flush_swap_buffers();
        let gpu = unsafe { (*self.vk).gpu };
        let inst_ptr = unsafe { (*self.vk_inst).instance.cast::<c_void>() };
        unsafe {
            if !gpu.is_null() {
                ffi::pl_gpu_finish(gpu);
            }
            for (_, mut pass) in self.ui_passes.drain() {
                ffi::pl_pass_destroy(gpu, &mut pass);
            }
            for (_, mut tex) in self.ui_textures.drain() {
                ffi::pl_tex_destroy(gpu, &mut tex);
            }
            for slot in &mut self.video_planes {
                if !slot.is_null() {
                    ffi::pl_tex_destroy(gpu, slot);
                }
            }
            if !self.cas_hook.is_null() {
                ffi::pl_mpv_user_shader_destroy(&mut self.cas_hook);
            }
            if !self.renderer.is_null() {
                ffi::pl_renderer_destroy(&mut self.renderer);
            }
            if !self.sw.is_null() {
                ffi::pl_swapchain_destroy(&mut self.sw);
            }
            if !self.vk.is_null() {
                ffi::pl_vulkan_destroy(&mut self.vk);
            }
            if !self.surface.is_null() {
                (self.destroy_surface)(inst_ptr, self.surface);
                self.surface = std::ptr::null_mut();
            }
            if !self.vk_inst.is_null() {
                ffi::pl_vk_inst_destroy(&mut self.vk_inst);
            }
            if !self.log.is_null() {
                ffi::pl_log_destroy(&mut self.log);
            }
        }
    }
}

fn build_swapchain_params(
    surface: ffi::VkSurfaceKHR,
    vsync: bool,
) -> ffi::pl_vulkan_swapchain_params {
    let mut params: ffi::pl_vulkan_swapchain_params = unsafe { std::mem::zeroed() };
    params.surface = surface;
    params.present_mode = if vsync {
        ffi::VkPresentModeKHR_VK_PRESENT_MODE_FIFO_KHR
    } else {
        ffi::VkPresentModeKHR_VK_PRESENT_MODE_IMMEDIATE_KHR
    };
    params.swapchain_depth = 2;
    params.allow_suboptimal = false;
    params
}

/// Computes target `(x0, y0, x1, y1)` rectangle in viewport pixel coordinates for a given `ScaleMode`.
pub fn compute_placebo_target_rect(
    frame_w: u32,
    frame_h: u32,
    view_w: u32,
    view_h: u32,
    scale_mode: ScaleMode,
) -> [f32; 4] {
    if frame_w == 0 || frame_h == 0 || view_w == 0 || view_h == 0 {
        return [0.0, 0.0, view_w as f32, view_h as f32];
    }
    let vw = view_w as f32;
    let vh = view_h as f32;
    if scale_mode == ScaleMode::Stretch {
        return [0.0, 0.0, vw, vh];
    }

    let frame_aspect = frame_w as f32 / frame_h as f32;
    let view_aspect = vw / vh;
    let (draw_w, draw_h) = match scale_mode {
        ScaleMode::Fit => {
            if view_aspect > frame_aspect {
                (vh * frame_aspect, vh)
            } else {
                (vw, vw / frame_aspect)
            }
        }
        ScaleMode::Zoom => {
            if view_aspect > frame_aspect {
                (vw, vw / frame_aspect)
            } else {
                (vh * frame_aspect, vh)
            }
        }
        ScaleMode::Stretch => unreachable!(),
    };

    let x0 = (vw - draw_w) * 0.5;
    let y0 = (vh - draw_h) * 0.5;
    [x0, y0, x0 + draw_w, y0 + draw_h]
}

unsafe extern "C" fn placebo_log_cb(
    _priv: *mut c_void,
    level: ffi::pl_log_level,
    msg: *const c_char,
) {
    if msg.is_null() {
        return;
    }
    let text = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    match level {
        ffi::pl_log_level_PL_LOG_FATAL | ffi::pl_log_level_PL_LOG_ERR => {
            tracing::error!("libplacebo: {text}");
        }
        ffi::pl_log_level_PL_LOG_WARN => {
            tracing::warn!("libplacebo: {text}");
        }
        ffi::pl_log_level_PL_LOG_INFO => {
            tracing::info!("libplacebo: {text}");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_placebo_target_rects_for_all_scale_modes() {
        // Exact 16:9 inside 16:9 viewport
        assert_eq!(
            compute_placebo_target_rect(1920, 1080, 1280, 720, ScaleMode::Fit),
            [0.0, 0.0, 1280.0, 720.0]
        );
        // 16:9 Fit inside 21:9 ultrawide (pillarboxed horizontally)
        let fit = compute_placebo_target_rect(1920, 1080, 2560, 1080, ScaleMode::Fit);
        assert!((fit[0] - 320.0).abs() < 0.01);
        assert_eq!(fit[1], 0.0);
        assert!((fit[2] - 2240.0).abs() < 0.01);
        assert_eq!(fit[3], 1080.0);

        // 16:9 Zoom inside 21:9 ultrawide (cropped vertically)
        let zoom = compute_placebo_target_rect(1920, 1080, 2560, 1080, ScaleMode::Zoom);
        assert_eq!(zoom[0], 0.0);
        assert!((zoom[1] - -180.0).abs() < 0.01);
        assert_eq!(zoom[2], 2560.0);
        assert!((zoom[3] - 1260.0).abs() < 0.01);

        // Stretch fills viewport unconditionally
        assert_eq!(
            compute_placebo_target_rect(1920, 1080, 2560, 1080, ScaleMode::Stretch),
            [0.0, 0.0, 2560.0, 1080.0]
        );
    }

    #[test]
    fn headless_libplacebo_vulkan_compiles_egui_glsl_pass() {
        let log_params = ffi::pl_log_params {
            log_cb: Some(placebo_log_cb),
            log_priv: std::ptr::null_mut(),
            log_level: ffi::pl_log_level_PL_LOG_WARN,
        };
        let mut log = unsafe { ffi::pl_log_create_387(ffi::PL_API_VER as c_int, &log_params) };
        assert!(!log.is_null(), "pl_log_create should succeed");

        let inst_params = unsafe { ffi::pl_vk_inst_default_params };
        let mut inst = unsafe { ffi::pl_vk_inst_create(log, &inst_params) };
        if inst.is_null() {
            unsafe { ffi::pl_log_destroy(&mut log) };
            eprintln!("skipping headless libplacebo pass test: no Vulkan ICD available");
            return;
        }

        let mut vk_params = unsafe { ffi::pl_vulkan_default_params };
        unsafe {
            vk_params.instance = (*inst).instance;
            vk_params.get_proc_addr = (*inst).get_proc_addr;
            vk_params.allow_software = true;
        }
        let mut vk = unsafe { ffi::pl_vulkan_create(log, &vk_params) };
        if vk.is_null() {
            unsafe {
                ffi::pl_vk_inst_destroy(&mut inst);
                ffi::pl_log_destroy(&mut log);
            }
            eprintln!("skipping headless libplacebo pass test: no Vulkan device available");
            return;
        }

        let gpu = unsafe { (*vk).gpu };
        let mut renderer = unsafe { ffi::pl_renderer_create(log, gpu) };
        assert!(!renderer.is_null(), "pl_renderer_create should succeed");

        let target_fmt = unsafe {
            ffi::pl_find_fmt(
                gpu,
                ffi::pl_fmt_type_PL_FMT_UNORM,
                4,
                8,
                8,
                (ffi::pl_fmt_caps_PL_FMT_CAP_RENDERABLE
                    | ffi::pl_fmt_caps_PL_FMT_CAP_BLENDABLE
                    | ffi::pl_fmt_caps_PL_FMT_CAP_SAMPLEABLE
                    | ffi::pl_fmt_caps_PL_FMT_CAP_LINEAR) as ffi::pl_fmt_caps,
            )
        };
        let vert_pos_fmt =
            unsafe { ffi::pl_find_vertex_fmt(gpu, ffi::pl_fmt_type_PL_FMT_FLOAT, 2) };
        let vert_color_fmt = unsafe {
            ffi::pl_find_fmt(
                gpu,
                ffi::pl_fmt_type_PL_FMT_UNORM,
                4,
                8,
                8,
                ffi::pl_fmt_caps_PL_FMT_CAP_VERTEX,
            )
        };
        assert!(!target_fmt.is_null());
        assert!(!vert_pos_fmt.is_null());
        assert!(!vert_color_fmt.is_null());

        let mut descs = [ffi::pl_desc {
            name: c"tex0".as_ptr(),
            type_: ffi::pl_desc_type_PL_DESC_SAMPLED_TEX,
            binding: 0,
            access: ffi::pl_desc_access_PL_DESC_ACCESS_READONLY,
        }];
        let mut attribs = [
            ffi::pl_vertex_attrib {
                name: c"in_pos".as_ptr(),
                fmt: vert_pos_fmt,
                offset: 0,
                location: 0,
            },
            ffi::pl_vertex_attrib {
                name: c"in_uv".as_ptr(),
                fmt: vert_pos_fmt,
                offset: 8,
                location: 1,
            },
            ffi::pl_vertex_attrib {
                name: c"in_color".as_ptr(),
                fmt: vert_color_fmt,
                offset: 16,
                location: 2,
            },
        ];
        let mut pass_params: ffi::pl_pass_params = unsafe { std::mem::zeroed() };
        pass_params.type_ = ffi::pl_pass_type_PL_PASS_RASTER;
        pass_params.descriptors = descs.as_mut_ptr();
        pass_params.num_descriptors = 1;
        pass_params.push_constants_size = std::mem::size_of::<UiPushConstants>();
        pass_params.glsl_shader = EGUI_PS_GLSL.as_ptr();
        pass_params.vertex_type = ffi::pl_prim_type_PL_PRIM_TRIANGLE_LIST;
        pass_params.vertex_attribs = attribs.as_mut_ptr();
        pass_params.num_vertex_attribs = 3;
        pass_params.vertex_stride = 20;
        pass_params.vertex_shader = EGUI_VS_GLSL.as_ptr();
        pass_params.target_format = target_fmt;
        pass_params.blend_params = &raw const ffi::pl_alpha_overlay;
        pass_params.load_target = true;

        let mut pass = unsafe { ffi::pl_pass_create(gpu, &pass_params) };
        assert!(
            !pass.is_null(),
            "egui overlay GLSL pl_pass_create should compile"
        );

        let mut cas_hook = unsafe {
            ffi::pl_mpv_user_shader_parse(gpu, CAS_HOOK_GLSL.as_ptr().cast(), CAS_HOOK_GLSL.len())
        };
        assert!(
            !cas_hook.is_null(),
            "CAS_HOOK_GLSL pl_mpv_user_shader_parse should succeed"
        );
        unsafe {
            assert!(
                (*cas_hook).num_parameters >= 1 && !(*cas_hook).parameters.is_null(),
                "CAS hook should expose dynamic sharpness parameter"
            );
            let data = (*(*cas_hook).parameters).data;
            assert!(
                !data.is_null(),
                "CAS parameter data pointer should be valid"
            );
            (*data).f = 0.5;
            assert!(((*data).f - 0.5).abs() < f32::EPSILON);

            let mut src_tex_params: ffi::pl_tex_params = std::mem::zeroed();
            src_tex_params.w = 4;
            src_tex_params.h = 4;
            src_tex_params.format = target_fmt;
            src_tex_params.sampleable = true;
            let mut src_tex = ffi::pl_tex_create(gpu, &src_tex_params);
            assert!(!src_tex.is_null(), "headless CAS src_tex create");

            let mut dst_tex_params: ffi::pl_tex_params = std::mem::zeroed();
            dst_tex_params.w = 4;
            dst_tex_params.h = 4;
            dst_tex_params.format = target_fmt;
            dst_tex_params.renderable = true;
            let mut dst_tex = ffi::pl_tex_create(gpu, &dst_tex_params);
            assert!(!dst_tex.is_null(), "headless CAS dst_tex create");

            let crop = ffi::pl_rect2df {
                x0: 0.0,
                y0: 0.0,
                x1: 4.0,
                y1: 4.0,
            };
            let mut src_frame: ffi::pl_frame = std::mem::zeroed();
            src_frame.num_planes = 1;
            src_frame.planes[0].texture = src_tex;
            src_frame.planes[0].components = 4;
            src_frame.planes[0].component_mapping = [0, 1, 2, 3];
            src_frame.crop = crop;
            src_frame.color = ffi::pl_color_space_srgb;
            src_frame.repr = ffi::pl_color_repr_rgb;

            let mut dst_frame: ffi::pl_frame = std::mem::zeroed();
            dst_frame.num_planes = 1;
            dst_frame.planes[0].texture = dst_tex;
            dst_frame.planes[0].components = 4;
            dst_frame.planes[0].component_mapping = [0, 1, 2, 3];
            dst_frame.crop = crop;
            dst_frame.color = ffi::pl_color_space_srgb;
            dst_frame.repr = ffi::pl_color_repr_rgb;

            let hook_ptrs = [cas_hook];
            let mut render_params = ffi::pl_render_fast_params;
            render_params.upscaler = &raw const ffi::pl_filter_spline36;
            render_params.hooks = hook_ptrs.as_ptr();
            render_params.num_hooks = 1;
            let ok = ffi::pl_render_image(renderer, &src_frame, &dst_frame, &render_params);
            assert!(ok, "pl_render_image with CAS_HOOK_GLSL should succeed");

            ffi::pl_gpu_finish(gpu);
            ffi::pl_tex_destroy(gpu, &mut src_tex);
            ffi::pl_tex_destroy(gpu, &mut dst_tex);
            ffi::pl_mpv_user_shader_destroy(&mut cas_hook);
            ffi::pl_pass_destroy(gpu, &mut pass);
            ffi::pl_renderer_destroy(&mut renderer);
            ffi::pl_vulkan_destroy(&mut vk);
            ffi::pl_vk_inst_destroy(&mut inst);
            ffi::pl_log_destroy(&mut log);
        }
    }
}
