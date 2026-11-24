use crate::egui_d3d11::{clip_rect_to_scissor, color_image_to_rgba8};
use anyhow::{Context as _, Result, bail};
use gnsh_media::{UiMeshDraw, UiTextureWrap, VulkanPlaceboRenderer};
use std::collections::HashMap;

pub(crate) struct EguiPlaceboPainter {
    texture_options: HashMap<egui::TextureId, egui::TextureOptions>,
    vertex_bytes_scratch: Vec<u8>,
}

impl EguiPlaceboPainter {
    pub fn new() -> Self {
        Self {
            texture_options: HashMap::new(),
            vertex_bytes_scratch: Vec::new(),
        }
    }

    pub fn update_textures(
        &mut self,
        renderer: &mut VulkanPlaceboRenderer,
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
                let tex_id = texture_id_key(*id);

                match image_delta.pos {
                    None => {
                        renderer.upsert_ui_texture(tex_id, width_u32, height_u32, &pixels)?;
                    }
                    Some([px, py]) => {
                        let px_u32 = u32::try_from(px).context("partial update x exceeds u32")?;
                        let py_u32 = u32::try_from(py).context("partial update y exceeds u32")?;
                        renderer.update_ui_texture_region(
                            tex_id, px_u32, py_u32, width_u32, height_u32, &pixels,
                        )?;
                    }
                }
                self.texture_options.insert(*id, image_delta.options);
            }
        }
        Ok(())
    }

    pub fn paint(
        &mut self,
        renderer: &mut VulkanPlaceboRenderer,
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
        let screen_size_pts = [screen_size_px.0 as f32 / ppp, screen_size_px.1 as f32 / ppp];
        let hdr_paper_white_nits = if hdr_output { 200.0 } else { 0.0 };

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

            let options = self
                .texture_options
                .get(&mesh.texture_id)
                .copied()
                .unwrap_or(egui::TextureOptions::LINEAR);
            pack_mesh_vertices(&mesh.vertices, &mut self.vertex_bytes_scratch);

            renderer.draw_ui_mesh(UiMeshDraw {
                tex_id: texture_id_key(mesh.texture_id),
                linear_filter: options.magnification == egui::TextureFilter::Linear,
                wrap: map_wrap_mode(options.wrap_mode),
                vertices_bytes: &self.vertex_bytes_scratch,
                indices: &mesh.indices,
                scissor: [scissor.left, scissor.top, scissor.right, scissor.bottom],
                screen_size_pts,
                hdr_paper_white_nits,
            })?;
        }

        Ok(())
    }

    pub fn free_textures(
        &mut self,
        renderer: &mut VulkanPlaceboRenderer,
        delta: &egui::TexturesDelta,
    ) {
        for id in &delta.free {
            self.texture_options.remove(id);
            renderer.free_ui_texture(texture_id_key(*id));
        }
    }
}

fn texture_id_key(id: egui::TextureId) -> u64 {
    match id {
        egui::TextureId::Managed(n) => n << 1,
        egui::TextureId::User(n) => (n << 1) | 1,
    }
}

fn map_wrap_mode(wrap: egui::TextureWrapMode) -> UiTextureWrap {
    match wrap {
        egui::TextureWrapMode::ClampToEdge => UiTextureWrap::ClampToEdge,
        egui::TextureWrapMode::Repeat => UiTextureWrap::Repeat,
        egui::TextureWrapMode::MirroredRepeat => UiTextureWrap::MirroredRepeat,
    }
}

fn pack_mesh_vertices(vertices: &[egui::epaint::Vertex], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(vertices.len() * 20);
    for v in vertices {
        out.extend_from_slice(&v.pos.x.to_ne_bytes());
        out.extend_from_slice(&v.pos.y.to_ne_bytes());
        out.extend_from_slice(&v.uv.x.to_ne_bytes());
        out.extend_from_slice(&v.uv.y.to_ne_bytes());
        out.extend_from_slice(&v.color.to_array());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_ids_and_vertex_packing_do_not_collide() {
        assert_ne!(
            texture_id_key(egui::TextureId::Managed(0)),
            texture_id_key(egui::TextureId::User(0))
        );
        assert_ne!(
            texture_id_key(egui::TextureId::Managed(1)),
            texture_id_key(egui::TextureId::User(1))
        );

        let v = egui::epaint::Vertex {
            pos: egui::pos2(12.5, 34.0),
            uv: egui::pos2(0.25, 0.75),
            color: egui::Color32::from_rgba_premultiplied(10, 20, 30, 40),
        };
        let mut buf = Vec::new();
        pack_mesh_vertices(&[v], &mut buf);
        assert_eq!(buf.len(), 20);
        assert_eq!(&buf[0..4], &12.5_f32.to_ne_bytes());
        assert_eq!(&buf[4..8], &34.0_f32.to_ne_bytes());
        assert_eq!(&buf[8..12], &0.25_f32.to_ne_bytes());
        assert_eq!(&buf[12..16], &0.75_f32.to_ne_bytes());
        assert_eq!(&buf[16..20], &[10, 20, 30, 40]);
    }
}
