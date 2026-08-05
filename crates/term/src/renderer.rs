//! wgpu 终端渲染器。
//!
//! 职责：
//! - 字形 atlas：把 `ab_glyph` 光栅化的字形位图打包进一张 R8 纹理，按 glyph_id 缓存。
//! - 网格构建：把 `alacritty_terminal` 的 `RenderableContent` 转成两组四边形
//!   （背景 / 字形），顶点携带 位置 + UV + 颜色。
//! - 绘制：单一管线 + 预乘 alpha 混合；背景矩形复用 atlas 中的 1×1 白像素。
//!
//! 坐标约定：网格以「物理像素」为单位构建（原点 = 终端视图左上角），绘制时由
//! `paint` 把绝对像素坐标变换到 NDC。

use std::collections::HashMap;
use std::sync::Arc;

use ab_glyph::{Font, FontArc, Glyph, PxScale, ScaleFont};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::RenderableContent;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};

// ---------------------------------------------------------------------------
// 常量与着色器
// ---------------------------------------------------------------------------

/// 字形 atlas 尺寸（R8）。
const ATLAS_SIZE: u32 = 1024;
/// atlas 打包边距。
const ATLAS_PADDING: u32 = 1;

/// 背景矩形采样用的 1×1 白像素中心 UV（零面积采样）。
const WHITE_UV: [f32; 4] = [
    0.5 / ATLAS_SIZE as f32,
    0.5 / ATLAS_SIZE as f32,
    0.5 / ATLAS_SIZE as f32,
    0.5 / ATLAS_SIZE as f32,
];

const SHADER: &str = r#"
struct Globals {
    transform: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VsIn {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_position = globals.transform * vec4<f32>(in.position, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // atlas 是 alpha 掩码（R 通道）；背景矩形采样 1×1 白像素 → mask = 1。
    let mask = textureSample(atlas, atlas_sampler, in.uv).r;
    // 预乘 alpha 输出，混合 ONE / ONE_MINUS_SRC_ALPHA。
    return vec4<f32>(in.color.rgb * mask, in.color.a * mask);
}
"#;

// ---------------------------------------------------------------------------
// 顶点与网格
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

/// CPU 侧构建的两组四边形。
#[derive(Default)]
struct Mesh {
    bg: Vec<Vertex>,
    glyph: Vec<Vertex>,
}

impl Mesh {
    fn add_quad(
        list: &mut Vec<Vertex>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        uv: [f32; 4],
        color: [f32; 4],
    ) {
        let (u0, v0, u1, v1) = (uv[0], uv[1], uv[2], uv[3]);
        let (x1, y1) = (x + w, y + h);
        let verts = [
            Vertex { position: [x, y], uv: [u0, v0], color },
            Vertex { position: [x1, y], uv: [u1, v0], color },
            Vertex { position: [x1, y1], uv: [u1, v1], color },
            Vertex { position: [x, y], uv: [u0, v0], color },
            Vertex { position: [x1, y1], uv: [u1, v1], color },
            Vertex { position: [x, y1], uv: [u0, v1], color },
        ];
        list.extend_from_slice(&verts);
    }
}

// ---------------------------------------------------------------------------
// 默认调色板
// ---------------------------------------------------------------------------

/// 默认终端配色（Term 未收到 OSC 4/10/11 前使用）。与 Mac 版 Stacio Dark 主题对齐。
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub fg: [f32; 3],
    pub bg: [f32; 3],
    pub cursor: [f32; 3],
    pub cursor_text: [f32; 3],
    pub selection: [f32; 3],
    pub ansi: [[f32; 3]; 16],
}

fn hex(hex: u32) -> [f32; 3] {
    [
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    ]
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            fg: hex(0xdcdfe4),
            bg: hex(0x282c34),
            cursor: hex(0x528bff),
            cursor_text: hex(0xffffff),
            selection: hex(0x3e4451),
            ansi: [
                hex(0x282c34),
                hex(0xe06c75),
                hex(0x98c379),
                hex(0xe5c07b),
                hex(0x61afef),
                hex(0xc678dd),
                hex(0x56b6c2),
                hex(0xabb2bf),
                hex(0x5c6370),
                hex(0xe06c75),
                hex(0x98c379),
                hex(0xe5c07b),
                hex(0x61afef),
                hex(0xc678dd),
                hex(0x56b6c2),
                hex(0xffffff),
            ],
        }
    }
}

impl Palette {
    /// 解析单元格颜色引用为具体 RGB。
    fn resolve(&self, colors: &Colors, color: &Color) -> Rgb {
        match color {
            Color::Spec(rgb) => *rgb,
            Color::Indexed(idx) => colors[*idx as usize].unwrap_or(self.indexed(*idx)),
            Color::Named(named) => colors[*named].unwrap_or(self.named(*named)),
        }
    }

    fn indexed(&self, idx: u8) -> Rgb {
        match idx {
            0..=15 => self.rgb(self.ansi[idx as usize]),
            16..=231 => {
                let idx = idx - 16;
                let r = idx / 36;
                let g = (idx % 36) / 6;
                let b = idx % 6;
                let cube = |v: u8| [0, 0x5f, 0x87, 0xaf, 0xd7, 0xff][v as usize];
                Rgb { r: cube(r), g: cube(g), b: cube(b) }
            }
            _ => {
                let v = 8 + (idx - 232) * 10;
                Rgb { r: v, g: v, b: v }
            }
        }
    }

    fn named(&self, named: NamedColor) -> Rgb {
        match named {
            NamedColor::Foreground => self.rgb(self.fg),
            NamedColor::Background => self.rgb(self.bg),
            NamedColor::Cursor => self.rgb(self.cursor),
            NamedColor::DimForeground => {
                self.rgb([self.fg[0] * 0.6, self.fg[1] * 0.6, self.fg[2] * 0.6])
            }
            NamedColor::BrightForeground => self.rgb(self.ansi[15]),
            NamedColor::Black => self.rgb(self.ansi[0]),
            NamedColor::Red => self.rgb(self.ansi[1]),
            NamedColor::Green => self.rgb(self.ansi[2]),
            NamedColor::Yellow => self.rgb(self.ansi[3]),
            NamedColor::Blue => self.rgb(self.ansi[4]),
            NamedColor::Magenta => self.rgb(self.ansi[5]),
            NamedColor::Cyan => self.rgb(self.ansi[6]),
            NamedColor::White => self.rgb(self.ansi[7]),
            NamedColor::BrightBlack => self.rgb(self.ansi[8]),
            NamedColor::BrightRed => self.rgb(self.ansi[9]),
            NamedColor::BrightGreen => self.rgb(self.ansi[10]),
            NamedColor::BrightYellow => self.rgb(self.ansi[11]),
            NamedColor::BrightBlue => self.rgb(self.ansi[12]),
            NamedColor::BrightMagenta => self.rgb(self.ansi[13]),
            NamedColor::BrightCyan => self.rgb(self.ansi[14]),
            NamedColor::BrightWhite => self.rgb(self.ansi[15]),
            _ => self.rgb(self.fg),
        }
    }

    fn rgb(&self, [r, g, b]: [f32; 3]) -> Rgb {
        Rgb {
            r: (r * 255.0) as u8,
            g: (g * 255.0) as u8,
            b: (b * 255.0) as u8,
        }
    }
}

fn rgb_to_f32(rgb: Rgb) -> [f32; 4] {
    [
        rgb.r as f32 / 255.0,
        rgb.g as f32 / 255.0,
        rgb.b as f32 / 255.0,
        1.0,
    ]
}

fn dim_color([r, g, b, a]: [f32; 4]) -> [f32; 4] {
    [r * 0.6, g * 0.6, b * 0.6, a]
}

// ---------------------------------------------------------------------------
// 字体与度量
// ---------------------------------------------------------------------------

/// 字体对：常规 + 加粗（PoC 加粗暂用合成粗体，后续接入真实粗体字体）。
pub struct FontPair {
    pub regular: FontArc,
    pub bold: FontArc,
}

impl FontPair {
    pub fn from_bytes(regular: Vec<u8>, bold: Option<Vec<u8>>) -> anyhow::Result<Self> {
        let regular = FontArc::try_from_vec(regular)?;
        let bold = match bold {
            Some(b) => FontArc::try_from_vec(b)?,
            None => regular.clone(),
        };
        Ok(Self { regular, bold })
    }
}

/// 由字体 + 物理像素字号推导的单元格度量。
#[derive(Debug, Clone, Copy)]
pub struct FontMetrics {
    /// 单元宽度（px）。
    pub cell_width: f32,
    /// 单元高度（px）。
    pub cell_height: f32,
    /// 基线相对单元顶部的偏移（px，Y 向下为正）。
    pub baseline_y: f32,
    /// 点字号。
    pub font_size: f32,
    /// 光栅化时的物理字号（含 DPI）。
    pub physical_size: f32,
}

impl FontMetrics {
    pub fn new(font: &FontArc, size_pt: f32, dpi_scale: f32) -> Self {
        let physical = (size_pt * dpi_scale).max(1.0);
        let scaled = font.as_scaled(physical);
        let ascent = scaled.ascent();
        let descent = scaled.descent();
        let line_gap = scaled.line_gap();

        let cell_height = (ascent - descent + line_gap).ceil().max(1.0);
        let adv = scaled.h_advance(scaled.glyph_id('M'));
        let cell_width = adv.ceil().max(1.0);
        // 基线：让行内 ink 近似垂直居中。
        let baseline_y = (ascent - descent) / 2.0 - descent + line_gap / 2.0;

        Self {
            cell_width,
            cell_height,
            baseline_y: baseline_y.max(1.0),
            font_size: size_pt,
            physical_size: physical,
        }
    }
}

// ---------------------------------------------------------------------------
// 字形 atlas
// ---------------------------------------------------------------------------

/// atlas 条目：字形位图 UV + 相对字形原点（基线）的 ink 偏移。
#[derive(Debug, Clone, Copy)]
struct AtlasEntry {
    uv: [f32; 4],
    /// ink 左上角相对基线的 X 偏移（px）。
    min_x: f32,
    /// ink 左上角相对基线的 Y 偏移（px，负值 = 基线上方）。
    min_y: f32,
    width: f32,
    height: f32,
}

struct GlyphAtlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    next_x: u32,
    next_y: u32,
    row_height: u32,
    cache: HashMap<ab_glyph::GlyphId, AtlasEntry>,
}

impl GlyphAtlas {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("stacio-glyph-atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let atlas = Self {
            texture,
            view,
            next_x: ATLAS_PADDING,
            next_y: ATLAS_PADDING,
            row_height: 0,
            cache: HashMap::new(),
        };
        // 预留 (0,0) 1×1 白像素，背景矩形采样它。
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &atlas.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(1),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        atlas
    }

    /// 取字形条目；未缓存则光栅化并上传。
    fn get(
        &mut self,
        queue: &wgpu::Queue,
        font: &FontArc,
        glyph_id: ab_glyph::GlyphId,
        physical_size: f32,
    ) -> AtlasEntry {
        if let Some(entry) = self.cache.get(&glyph_id) {
            return *entry;
        }
        let entry = self.rasterize(queue, font, glyph_id, physical_size);
        self.cache.insert(glyph_id, entry);
        entry
    }

    fn rasterize(
        &mut self,
        queue: &wgpu::Queue,
        font: &FontArc,
        glyph_id: ab_glyph::GlyphId,
        physical_size: f32,
    ) -> AtlasEntry {
        let glyph = Glyph {
            id: glyph_id,
            scale: PxScale::from(physical_size),
            position: ab_glyph::point(0.0, 0.0),
        };

        let outlined = match font.outline_glyph(glyph) {
            Some(o) => o,
            None => {
                return AtlasEntry {
                    uv: WHITE_UV,
                    min_x: 0.0,
                    min_y: 0.0,
                    width: 0.0,
                    height: 0.0,
                };
            }
        };

        let bounds = outlined.px_bounds();
        let width = bounds.width().ceil().max(1.0) as u32;
        let height = bounds.height().ceil().max(1.0) as u32;

        let mut bitmap = vec![0u8; (width * height) as usize];
        outlined.draw(|x, y, coverage| {
            let (x, y) = (x as usize, y as usize);
            if x < width as usize && y < height as usize {
                bitmap[y * width as usize + x] = (coverage * 255.0).round() as u8;
            }
        });

        // 逐行打包。
        let needed_row_height = height + ATLAS_PADDING * 2;
        if self.next_x + width + ATLAS_PADDING > ATLAS_SIZE {
            self.next_x = ATLAS_PADDING;
            self.next_y += self.row_height.max(1);
            self.row_height = 0;
        }
        self.row_height = self.row_height.max(needed_row_height);

        let (x0, y0) = (self.next_x, self.next_y);
        self.next_x += width + ATLAS_PADDING;

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &bitmap,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        let inv = 1.0 / ATLAS_SIZE as f32;
        AtlasEntry {
            uv: [
                x0 as f32 * inv,
                y0 as f32 * inv,
                (x0 + width) as f32 * inv,
                (y0 + height) as f32 * inv,
            ],
            min_x: bounds.min.x,
            min_y: bounds.min.y,
            width: width as f32,
            height: height as f32,
        }
    }
}

// ---------------------------------------------------------------------------
// 终端渲染器
// ---------------------------------------------------------------------------

pub struct TerminalRenderer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,

    palette: Palette,
    fonts: FontPair,
    metrics: FontMetrics,
    atlas: GlyphAtlas,

    bg_buffer: wgpu::Buffer,
    glyph_buffer: wgpu::Buffer,
    bg_len: usize,
    glyph_len: usize,
}

impl TerminalRenderer {
    pub fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        target_format: wgpu::TextureFormat,
        fonts: FontPair,
        size_pt: f32,
        dpi_scale: f32,
    ) -> Self {
        let metrics = FontMetrics::new(&fonts.regular, size_pt, dpi_scale);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stacio-terminal-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stacio-terminal-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("stacio-terminal-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let atlas = GlyphAtlas::new(&device, &queue);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("stacio-terminal-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stacio-terminal-uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stacio-terminal-bind-group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("stacio-terminal-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 2,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let bg_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stacio-terminal-bg"),
            size: 4096,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let glyph_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stacio-terminal-glyph"),
            size: 4096,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            device,
            queue,
            pipeline,
            uniform_buffer,
            bind_group,
            palette: Palette::default(),
            fonts,
            metrics,
            atlas,
            bg_buffer,
            glyph_buffer,
            bg_len: 0,
            glyph_len: 0,
        }
    }

    /// 字号 / DPI 变化时重新计算度量并清空 atlas。
    pub fn set_font_size(&mut self, size_pt: f32, dpi_scale: f32) {
        self.metrics = FontMetrics::new(&self.fonts.regular, size_pt, dpi_scale);
        self.atlas = GlyphAtlas::new(&self.device, &self.queue);
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        self.bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stacio-terminal-bind-group"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
    }

    pub fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    /// 终端网格的自然像素尺寸。
    pub fn grid_pixel_size(&self, cols: usize, rows: usize) -> (f32, f32) {
        (
            cols as f32 * self.metrics.cell_width,
            rows as f32 * self.metrics.cell_height,
        )
    }

    /// 从渲染内容重建网格并上传 GPU 数据。
    pub fn prepare(&mut self, content: RenderableContent, cols: usize, rows: usize) {
        let mesh = self.build_mesh(content, cols, rows);
        self.upload_mesh(&mesh);
    }

    /// 构建四边形网格（纯 CPU）。
    fn build_mesh(&mut self, content: RenderableContent, cols: usize, rows: usize) -> Mesh {
        let RenderableContent {
            display_iter,
            selection,
            cursor,
            display_offset,
            colors,
            ..
        } = content;

        let mut mesh = Mesh::default();
        let palette = self.palette;
        let metrics = self.metrics;
        let (cw, ch) = (metrics.cell_width, metrics.cell_height);

        // 整屏背景底色。
        let screen_bg = palette.resolve(colors, &Color::Named(NamedColor::Background));
        let screen_bg_f32 = rgb_to_f32(screen_bg);
        Mesh::add_quad(
            &mut mesh.bg,
            0.0,
            0.0,
            cols as f32 * cw,
            rows as f32 * ch,
            WHITE_UV,
            screen_bg_f32,
        );

        for idx in display_iter {
            let point = idx.point;
            let cell = &idx.cell;
            let x = point.column.0 as f32 * cw;
            let y = point.line.0 as f32 * ch;

            let mut fg = cell.fg;
            let mut cell_bg = cell.bg;
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut cell_bg);
            }

            let fg_rgb = palette.resolve(colors, &fg);
            let bg_rgb = palette.resolve(colors, &cell_bg);

            // 背景：仅当与屏幕底色不同才发矩形。
            if bg_rgb != screen_bg {
                let mut color = rgb_to_f32(bg_rgb);
                if cell.flags.contains(Flags::DIM) {
                    color = dim_color(color);
                }
                Mesh::add_quad(&mut mesh.bg, x, y, cw, ch, WHITE_UV, color);
            }

            // 字形。
            let c = cell.c;
            if c != ' ' && c != '\0' && !cell.flags.contains(Flags::HIDDEN) {
                let bold = cell.flags.contains(Flags::BOLD);
                let font = if bold { &self.fonts.bold } else { &self.fonts.regular };
                let glyph_id = font.glyph_id(c);
                let entry = self.atlas.get(&self.queue, font, glyph_id, metrics.physical_size);
                if entry.width > 0.0 && entry.height > 0.0 {
                    let mut color = rgb_to_f32(fg_rgb);
                    if cell.flags.contains(Flags::DIM) {
                        color = dim_color(color);
                    }
                    let gx = x + entry.min_x;
                    let gy = y + metrics.baseline_y + entry.min_y;
                    Mesh::add_quad(&mut mesh.glyph, gx, gy, entry.width, entry.height, entry.uv, color);
                    // 合成粗体：右移 1px 再画一遍。
                    if bold {
                        Mesh::add_quad(&mut mesh.glyph, gx + 1.0, gy, entry.width, entry.height, entry.uv, color);
                    }
                }
            }
        }

        // 光标：仅当视口在底部（未滚动）且光标非隐藏时绘制。
        if display_offset == 0 && cursor.shape != alacritty_terminal::vte::ansi::CursorShape::Hidden {
            self.add_cursor(&mut mesh, cursor, colors, cw, ch);
        }

        // 选区。
        if let Some(sel) = &selection {
            self.add_selection(&mut mesh, sel, cols, cw, ch);
        }

        mesh
    }

    fn add_cursor(&self, mesh: &mut Mesh, cursor: alacritty_terminal::term::RenderableCursor, colors: &Colors, cw: f32, ch: f32) {
        let palette = self.palette;
        let cursor_color = rgb_to_f32(palette.resolve(colors, &Color::Named(NamedColor::Cursor)));
        let x = cursor.point.column.0 as f32 * cw;
        let y = cursor.point.line.0 as f32 * ch;
        match cursor.shape {
            CursorShape::Block => {
                Mesh::add_quad(&mut mesh.bg, x, y, cw, ch, WHITE_UV, cursor_color);
            }
            CursorShape::Underline => {
                Mesh::add_quad(&mut mesh.bg, x, y + ch - 2.0, cw, 2.0, WHITE_UV, cursor_color);
            }
            CursorShape::Beam => {
                Mesh::add_quad(&mut mesh.bg, x, y, 2.0, ch, WHITE_UV, cursor_color);
            }
            _ => {}
        }
    }

    fn add_selection(
        &self,
        mesh: &mut Mesh,
        sel: &alacritty_terminal::selection::SelectionRange,
        cols: usize,
        cw: f32,
        ch: f32,
    ) {
        let sel_color = rgb_to_f32(self.palette.rgb(self.palette.selection));
        for line in sel.start.line.0..=sel.end.line.0 {
            let y = line as f32 * ch;
            // 简化：PoC 阶段按整行高亮选区。
            Mesh::add_quad(&mut mesh.bg, 0.0, y, cols as f32 * cw, ch, WHITE_UV, sel_color);
        }
    }
}

impl TerminalRenderer {
    /// 上传网格到 GPU 缓冲。
    fn upload_mesh(&mut self, mesh: &Mesh) {
        let bg_bytes = bytemuck::cast_slice(&mesh.bg);
        let glyph_bytes = bytemuck::cast_slice(&mesh.glyph);

        if bg_bytes.len() as u64 > self.bg_buffer.size() {
            self.bg_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stacio-terminal-bg"),
                size: (bg_bytes.len() as u64).next_power_of_two(),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if glyph_bytes.len() as u64 > self.glyph_buffer.size() {
            self.glyph_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stacio-terminal-glyph"),
                size: (glyph_bytes.len() as u64).next_power_of_two(),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }

        if !bg_bytes.is_empty() {
            self.queue.write_buffer(&self.bg_buffer, 0, bg_bytes);
        }
        if !glyph_bytes.is_empty() {
            self.queue.write_buffer(&self.glyph_buffer, 0, glyph_bytes);
        }
        self.bg_len = mesh.bg.len();
        self.glyph_len = mesh.glyph.len();
    }

    /// 绘制。`viewport_points` = egui 视口 [min.x, min.y, w, h]（points），
    /// `origin_px` = 终端视图左上角在屏幕上的绝对像素坐标，`clip_points` = 裁剪矩形（points）。
    ///
    /// 网格顶点以终端左上角为原点、物理像素为单位；这里把绝对像素坐标变换到 NDC。
    pub fn paint(
        &self,
        pass: &mut wgpu::RenderPass,
        viewport_points: [f32; 4],
        origin_px: [f32; 2],
        pixels_per_point: f32,
        clip_points: [f32; 4],
    ) {
        let (vx, vy, vw, vh) = (
            viewport_points[0],
            viewport_points[1],
            viewport_points[2],
            viewport_points[3],
        );
        let (ox, oy) = (origin_px[0], origin_px[1]);
        let ppi = pixels_per_point;
        if vw <= 0.0 || vh <= 0.0 {
            return;
        }

        // 绝对像素 → 点 → NDC：
        //   pt_x = (ox + px) / ppi
        //   clip_x = (pt_x - vx) / vw * 2 - 1
        let m00 = 2.0 / (vw * ppi);
        let m03 = 2.0 * (ox / ppi - vx) / vw - 1.0;
        let m11 = -2.0 / (vh * ppi);
        let m13 = -2.0 * (oy / ppi - vy) / vh + 1.0;

        let matrix: [[f32; 4]; 4] = [
            // WGSL mat4x4 为列主序：每行即一列。平移分量在最后一列。
            [m00, 0.0, 0.0, 0.0],
            [0.0, m11, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [m03, m13, 0.0, 1.0],
        ];
        self.queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&matrix));

        // 裁剪到终端区域（物理像素）。
        let clip_x = (clip_points[0] * ppi) as u32;
        let clip_y = (clip_points[1] * ppi) as u32;
        let clip_w = ((clip_points[2] - clip_points[0]) * ppi) as u32;
        let clip_h = ((clip_points[3] - clip_points[1]) * ppi) as u32;
        if clip_w == 0 || clip_h == 0 {
            return;
        }
        pass.set_scissor_rect(clip_x, clip_y, clip_w, clip_h);

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);

        if self.bg_len > 0 {
            pass.set_vertex_buffer(0, self.bg_buffer.slice(..));
            pass.draw(0..self.bg_len as u32, 0..1);
        }
        if self.glyph_len > 0 {
            pass.set_vertex_buffer(0, self.glyph_buffer.slice(..));
            pass.draw(0..self.glyph_len as u32, 0..1);
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_resolves_basic_colors() {
        let palette = Palette::default();
        let colors = Colors::default();
        let fg = palette.resolve(&colors, &Color::Named(NamedColor::Red));
        assert_eq!(fg.r, 0xe0);
        assert_eq!(fg.g, 0x6c);
        let spec = palette.resolve(&colors, &Color::Spec(Rgb { r: 1, g: 2, b: 3 }));
        assert_eq!(spec.r, 1);
        assert_eq!(palette.indexed(196), Rgb { r: 255, g: 0, b: 0 });
        // 色立方中段：231 = 白（6×6×6 最后一个）。
        assert_eq!(palette.indexed(231), Rgb { r: 255, g: 255, b: 255 });
    }

    #[test]
    fn mesh_quad_generates_six_vertices() {
        let mut list = Vec::new();
        Mesh::add_quad(&mut list, 0.0, 0.0, 10.0, 20.0, [0.0, 0.0, 1.0, 1.0], [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(list.len(), 6);
        assert_eq!(list[0].position, [0.0, 0.0]);
        assert_eq!(list[2].position, [10.0, 20.0]);
    }
}
