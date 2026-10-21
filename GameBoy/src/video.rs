//! GBA renderer for text/affine background modes 0–2 and bitmap modes 3–5.
//!
//! This module renders the current video-memory snapshot, including tiled text
//! and affine backgrounds, regular OBJ sprites, and bitmap modes. Scanline timing
//! and DISPSTAT-driven IRQ generation are advanced by the memory bus.

use crate::MemoryBus;

/// Width of the GBA display in pixels.
pub const SCREEN_WIDTH: usize = 240;
/// Height of the GBA display in pixels.
pub const SCREEN_HEIGHT: usize = 160;
const VRAM_BASE: u32 = 0x0600_0000;
const PALETTE_BASE: u32 = 0x0500_0000;
const MODE_5_WIDTH: usize = 160;
const MODE_5_HEIGHT: usize = 128;
const BG0CNT: u32 = 0x0400_0008;
const BG0HOFS: u32 = 0x0400_0010;
const BG0VOFS: u32 = 0x0400_0012;

/// An opaque 240×160 frame in packed ARGB format (0xFFRRGGBB).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoFrame {
    /// Frame width in pixels.
    pub width: usize,
    /// Frame height in pixels.
    pub height: usize,
    /// Row-major opaque pixels, each packed as 0xFFRRGGBB.
    pub pixels: Vec<u32>,
}

impl VideoFrame {
    /// Consumes the frame and returns its row-major ARGB pixel buffer.
    ///
    /// This transfers ownership without cloning the framebuffer, which is
    /// useful when handing a completed frame to a frontend or image encoder.
    pub fn into_pixels(self) -> Vec<u32> {
        self.pixels
    }

    /// Converts the frame to tightly packed RGB888 bytes in row-major order.
    ///
    /// The source pixels use 0xFFRRGGBB; alpha is omitted. Returns None for
    /// malformed public frame values instead of silently truncating the image.
    pub fn to_rgb888(&self) -> Option<Vec<u8>> {
        if !self.is_well_formed() {
            return None;
        }
        let byte_count = self.pixels.len().checked_mul(3)?;
        let mut bytes = Vec::with_capacity(byte_count);
        for &pixel in &self.pixels {
            bytes.push((pixel >> 16) as u8);
            bytes.push((pixel >> 8) as u8);
            bytes.push(pixel as u8);
        }
        Some(bytes)
    }

    /// Converts the frame to tightly packed RGBA8888 bytes in row-major order.
    ///
    /// The source pixels use 0xFFRRGGBB, so the output order is R, G, B, A
    /// and alpha is always opaque (255). Malformed frames return None.
    pub fn to_rgba8888(&self) -> Option<Vec<u8>> {
        if !self.is_well_formed() {
            return None;
        }
        let byte_count = self.pixels.len().checked_mul(4)?;
        let mut bytes = Vec::with_capacity(byte_count);
        for &pixel in &self.pixels {
            bytes.push((pixel >> 16) as u8);
            bytes.push((pixel >> 8) as u8);
            bytes.push(pixel as u8);
            bytes.push(0xFF);
        }
        Some(bytes)
    }

    /// Converts the frame to tightly packed BGRA8888 bytes in row-major order.
    ///
    /// This is useful for common desktop surfaces that expect blue first.
    /// The output order is B, G, R, A with opaque alpha; malformed frames
    /// return None.
    pub fn to_bgra8888(&self) -> Option<Vec<u8>> {
        if !self.is_well_formed() {
            return None;
        }
        let byte_count = self.pixels.len().checked_mul(4)?;
        let mut bytes = Vec::with_capacity(byte_count);
        for &pixel in &self.pixels {
            bytes.push(pixel as u8);
            bytes.push((pixel >> 8) as u8);
            bytes.push((pixel >> 16) as u8);
            bytes.push(0xFF);
        }
        Some(bytes)
    }

    /// Converts the frame to little-endian RGB565 pixel bytes.
    ///
    /// Each source channel is truncated to 5 red, 6 green, and 5 blue bits.
    /// Pixels are emitted least-significant byte first; malformed frames
    /// return None.
    pub fn to_rgb565_le(&self) -> Option<Vec<u8>> {
        if !self.is_well_formed() {
            return None;
        }
        let byte_count = self.pixels.len().checked_mul(2)?;
        let mut bytes = Vec::with_capacity(byte_count);
        for &pixel in &self.pixels {
            let red = (pixel >> 19) & 0x1F;
            let green = (pixel >> 10) & 0x3F;
            let blue = (pixel >> 3) & 0x1F;
            let packed = ((red << 11) | (green << 5) | blue) as u16;
            bytes.extend_from_slice(&packed.to_le_bytes());
        }
        Some(bytes)
    }

    /// Returns whether the dimensions exactly match the backing pixel buffer.
    ///
    /// The check uses checked multiplication so malformed public frame values
    /// cannot overflow while validating their dimensions.
    pub fn is_well_formed(&self) -> bool {
        self.width
            .checked_mul(self.height)
            .is_some_and(|pixel_count| pixel_count == self.pixels.len())
    }

    /// Returns the pixel at (x, y), or None when outside the frame, when the
    /// public dimensions would overflow, or when the backing buffer is incomplete.
    pub fn pixel(&self, x: usize, y: usize) -> Option<u32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = y.checked_mul(self.width)?.checked_add(x)?;
        self.pixels.get(index).copied()
    }

    /// Returns one complete row, or None if the row is outside the frame or
    /// the backing buffer does not contain every pixel in that row.
    pub fn row(&self, y: usize) -> Option<&[u32]> {
        if y >= self.height {
            return None;
        }
        let start = y.checked_mul(self.width)?;
        let end = start.checked_add(self.width)?;
        self.pixels.get(start..end)
    }

    /// Returns one complete mutable row, or None if the row is outside the
    /// frame or the backing buffer does not contain every pixel in that row.
    pub fn row_mut(&mut self, y: usize) -> Option<&mut [u32]> {
        if y >= self.height {
            return None;
        }
        let start = y.checked_mul(self.width)?;
        let end = start.checked_add(self.width)?;
        self.pixels.get_mut(start..end)
    }

    /// Sets a pixel and returns false if its coordinates or backing-buffer
    /// offset are invalid. The frame's public dimensions are not modified.
    pub fn set_pixel(&mut self, x: usize, y: usize, color: u32) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let Some(index) = y
            .checked_mul(self.width)
            .and_then(|base| base.checked_add(x))
        else {
            return false;
        };
        if let Some(pixel) = self.pixels.get_mut(index) {
            *pixel = color;
            true
        } else {
            false
        }
    }
}

/// Renders the current GBA bitmap display state.
///
/// Modes 0–2 composite tiled text/affine backgrounds and regular OBJ sprites.
/// Mode 3 uses direct 15-bit color, mode 4 uses 8-bit palette indices, and
/// mode 5 uses direct color in a centered 160×128 viewport. Modes 6+ return
/// None unless forced blank is active. Forced blank returns an opaque white frame.
pub fn render_bitmap_frame(bus: &MemoryBus) -> Option<VideoFrame> {
    let display_control = bus.read16(0x0400_0000);
    let mode = display_control & 7;
    let forced_blank = display_control & (1 << 7) != 0;
    let mut windows = WindowConfig::from_bus(bus);
    if !forced_blank && mode > 5 {
        return None;
    }

    let mut frame = VideoFrame {
        width: SCREEN_WIDTH,
        height: SCREEN_HEIGHT,
        pixels: vec![0xFF00_0000; SCREEN_WIDTH * SCREEN_HEIGHT],
    };

    if forced_blank {
        frame.pixels.fill(0xFFFF_FFFF);
        return Some(frame);
    }

    // Sprite character data is shared across all priority passes in this frame.
    // Decode each referenced 8x8 tile at most once, including when multiple
    // sprites reuse the same tile or priority passes revisit it.
    let mut obj_tile_cache = vec![None::<[u8; 64]>; 1024];
    // OAM and affine matrices are stable during this snapshot render. Read
    // the 1 KiB once rather than rereading attributes on every priority pass.
    let oam_snapshot: [u8; 1024] =
        std::array::from_fn(|index| bus.read8(0x0700_0000 + index as u32));
    let allowed_lines = object_scanline_mask(&oam_snapshot);

    // WINOBJ is a pixel-accurate mask, not the sprite bounding box. Reuse the
    // normal OBJ sampler so affine transforms, flips, mosaic, tile addressing,
    // transparency, and the 32-OBJ-per-scanline limit match visible sprites.
    if windows.obj_window_enabled && display_control & (1 << 12) != 0 {
        let obj_palette = obj_palette_snapshot(bus);
        let mut mask_frame = VideoFrame {
            width: SCREEN_WIDTH,
            height: SCREEN_HEIGHT,
            pixels: vec![0; SCREEN_WIDTH * SCREEN_HEIGHT],
        };
        let mask_windows = windows.clone();
        for priority in (0..=3).rev() {
            render_regular_objects(
                bus,
                display_control,
                priority,
                &obj_palette,
                &mut obj_tile_cache,
                &oam_snapshot,
                &allowed_lines,
                None,
                None,
                Some(&mut windows.obj_window_mask),
                &mask_windows,
                &mut mask_frame,
            );
        }
    }

    // Bitmap modes use BG2 as their framebuffer plane. If BG2 is disabled,
    // show the backdrop color instead; OBJ pixels can still appear over it.
    if mode >= 3 && display_control & (1 << 10) == 0 {
        fill_backdrop(
            &mut frame,
            rgb555(bus.read16(PALETTE_BASE)),
            bitmap_brightness_effect(bus, display_control, 5),
            &windows,
        );
        let obj_palette = obj_palette_snapshot(bus);
        for priority in (0..=3).rev() {
            render_regular_objects(
                bus,
                display_control,
                priority,
                &obj_palette,
                &mut obj_tile_cache,
                &oam_snapshot,
                &allowed_lines,
                bitmap_backdrop_alpha_blend(bus),
                bitmap_brightness_effect(bus, display_control, 4),
                None,
                &windows,
                &mut frame,
            );
        }
        return Some(frame);
    }

    let bitmap_effect = if mode >= 3 {
        bitmap_brightness_effect(bus, display_control, 2)
    } else {
        None
    };

    match mode {
        0..=2 => render_text_backgrounds(
            bus,
            display_control,
            &mut obj_tile_cache,
            &oam_snapshot,
            &allowed_lines,
            &windows,
            &mut frame,
        ),
        3 => {
            let bg2_control = bus.read16(BG0CNT + 4);
            let mosaic_enabled = bg2_control & (1 << 6) != 0;
            let mosaic = bus.read16(0x0400_004C);
            let mosaic_width = usize::from((mosaic & 0xF) + 1);
            let mosaic_height = usize::from(((mosaic >> 4) & 0xF) + 1);
            let transform = BitmapTransform::new(
                bus,
                SCREEN_WIDTH,
                SCREEN_HEIGHT,
                mosaic_enabled,
                mosaic_width,
                mosaic_height,
                bg2_control & (1 << 13) != 0,
            );
            fill_backdrop(
                &mut frame,
                rgb555(bus.read16(PALETTE_BASE)),
                bitmap_brightness_effect(bus, display_control, 5),
                &windows,
            );
            for y in 0..SCREEN_HEIGHT {
                for x in 0..SCREEN_WIDTH {
                    let Some((source_x, source_y)) = transform.source_coordinates(x, y) else {
                        continue;
                    };
                    let address = VRAM_BASE + ((source_y * SCREEN_WIDTH + source_x) * 2) as u32;
                    if windows.allows(x, y, 2) {
                        frame.pixels[y * SCREEN_WIDTH + x] = apply_brightness_effect(
                            rgb555(bus.read16(address)),
                            if windows.effects_allowed(x, y) {
                                bitmap_effect
                            } else {
                                None
                            },
                        );
                    }
                }
            }
        }
        4 => {
            let page_offset = if display_control & (1 << 4) != 0 {
                0xA000
            } else {
                0
            };
            let bg2_control = bus.read16(BG0CNT + 4);
            let mosaic_enabled = bg2_control & (1 << 6) != 0;
            let mosaic = bus.read16(0x0400_004C);
            let mosaic_width = usize::from((mosaic & 0xF) + 1);
            let mosaic_height = usize::from(((mosaic >> 4) & 0xF) + 1);
            let transform = BitmapTransform::new(
                bus,
                SCREEN_WIDTH,
                SCREEN_HEIGHT,
                mosaic_enabled,
                mosaic_width,
                mosaic_height,
                bg2_control & (1 << 13) != 0,
            );
            // Palette RAM is unchanged during this snapshot render. Resolve it
            // once rather than issuing another bus read for every framebuffer pixel.
            let palette = palette_snapshot(bus);
            fill_backdrop(
                &mut frame,
                palette[0],
                bitmap_brightness_effect(bus, display_control, 5),
                &windows,
            );
            for y in 0..SCREEN_HEIGHT {
                for x in 0..SCREEN_WIDTH {
                    let Some((source_x, source_y)) = transform.source_coordinates(x, y) else {
                        continue;
                    };
                    let address =
                        VRAM_BASE + page_offset + (source_y * SCREEN_WIDTH + source_x) as u32;
                    let index = usize::from(bus.read8(address));
                    if windows.allows(x, y, 2) {
                        frame.pixels[y * SCREEN_WIDTH + x] = apply_brightness_effect(
                            palette[index],
                            if windows.effects_allowed(x, y) {
                                bitmap_effect
                            } else {
                                None
                            },
                        );
                    }
                }
            }
        }
        5 => {
            let page_offset = if display_control & (1 << 4) != 0 {
                0xA000
            } else {
                0
            };
            // Mode 5 displays only a centered 160×128 bitmap. Pixels outside
            // that viewport show the backdrop color (palette entry zero), not
            // an unconditional black fill.
            fill_backdrop(
                &mut frame,
                rgb555(bus.read16(PALETTE_BASE)),
                bitmap_brightness_effect(bus, display_control, 5),
                &windows,
            );
            let bg2_control = bus.read16(BG0CNT + 4);
            let mosaic_enabled = bg2_control & (1 << 6) != 0;
            let mosaic = bus.read16(0x0400_004C);
            let mosaic_width = usize::from((mosaic & 0xF) + 1);
            let mosaic_height = usize::from(((mosaic >> 4) & 0xF) + 1);
            let transform = BitmapTransform::new(
                bus,
                MODE_5_WIDTH,
                MODE_5_HEIGHT,
                mosaic_enabled,
                mosaic_width,
                mosaic_height,
                bg2_control & (1 << 13) != 0,
            );
            for y in 0..MODE_5_HEIGHT {
                for x in 0..MODE_5_WIDTH {
                    let Some((source_x, source_y)) = transform.source_coordinates(x, y) else {
                        continue;
                    };
                    let address =
                        VRAM_BASE + page_offset + ((source_y * MODE_5_WIDTH + source_x) * 2) as u32;
                    let target = (y + (SCREEN_HEIGHT - MODE_5_HEIGHT) / 2) * SCREEN_WIDTH
                        + x
                        + (SCREEN_WIDTH - MODE_5_WIDTH) / 2;
                    let screen_x = x + (SCREEN_WIDTH - MODE_5_WIDTH) / 2;
                    let screen_y = y + (SCREEN_HEIGHT - MODE_5_HEIGHT) / 2;
                    if windows.allows(screen_x, screen_y, 2) {
                        frame.pixels[target] = apply_brightness_effect(
                            rgb555(bus.read16(address)),
                            if windows.effects_allowed(screen_x, screen_y) {
                                bitmap_effect
                            } else {
                                None
                            },
                        );
                    }
                }
            }
        }
        _ => return None,
    }

    if mode >= 3 {
        // Bitmap modes reserve VRAM through 0x06013FFF for the framebuffer;
        // OBJ character data therefore begins at 0x06014000. BG2 priority is
        // still controlled by BG2CNT, so only OBJ priorities numerically less
        // than or equal to BG2's priority may cover the bitmap.
        let bg2_priority = bus.read16(BG0CNT + 4) & 3;
        let obj_palette = obj_palette_snapshot(bus);
        for priority in (0..=bg2_priority).rev() {
            render_regular_objects(
                bus,
                display_control,
                priority,
                &obj_palette,
                &mut obj_tile_cache,
                &oam_snapshot,
                &allowed_lines,
                bitmap_alpha_blend(bus, display_control),
                bitmap_brightness_effect(bus, display_control, 4),
                None,
                &windows,
                &mut frame,
            );
        }
    }

    Some(frame)
}

/// Composites backgrounds and regular OBJ sprites from back to front.
/// Modes 1 and 2 use affine backgrounds for BG2 and BG3 as specified by DISPCNT.
fn render_text_backgrounds(
    bus: &MemoryBus,
    display_control: u16,
    obj_tile_cache: &mut [Option<[u8; 64]>],
    oam_snapshot: &[u8; 1024],
    allowed_lines: &[[bool; SCREEN_HEIGHT]; 128],
    windows: &WindowConfig,
    frame: &mut VideoFrame,
) {
    let palette = palette_snapshot(bus);
    let obj_palette = obj_palette_snapshot(bus);
    fill_backdrop(
        frame,
        palette[0],
        bitmap_brightness_effect(bus, display_control, 5),
        windows,
    );
    for priority in (0..=3).rev() {
        // On equal priorities BG0 wins over BG1, then BG2, then BG3.
        // Draw the lower-numbered background last so it remains in front.
        for bg in (0..4).rev() {
            let control_address = BG0CNT + bg * 2;
            let control = bus.read16(control_address);
            let mode = display_control & 7;
            // Mode 2 exposes only affine BG2/BG3. BG0 and BG1 are not
            // available in this mode even if software leaves their enable bits set.
            if (mode == 2 && bg < 2)
                || display_control & (1 << (8 + bg)) == 0
                || control & 3 != priority
            {
                continue;
            }
            if mode == 0 || bg < 2 {
                render_text_background(
                    bus,
                    bg,
                    control,
                    &palette,
                    bitmap_brightness_effect(bus, display_control, bg as u16),
                    windows,
                    frame,
                );
            } else if bg >= 2 && (mode == 2 || bg == 2) {
                render_affine_background(
                    bus,
                    bg,
                    control,
                    &palette,
                    bitmap_brightness_effect(bus, display_control, bg as u16),
                    windows,
                    frame,
                );
            }
        }
        // OBJ pixels have precedence over BG pixels at the same priority.
        render_regular_objects(
            bus,
            display_control,
            priority,
            &obj_palette,
            obj_tile_cache,
            oam_snapshot,
            allowed_lines,
            None,
            bitmap_brightness_effect(bus, display_control, 4),
            None,
            windows,
            frame,
        );
    }
}

/// Renders an affine 8bpp tiled background (BG2/BG3 in modes 1 and 2).
/// Reference coordinates are signed 28-bit 8.8 fixed-point values; PA/PB/PC/PD
/// are signed 8.8 coefficients. The overflow bit controls wraparound outside
/// the affine background's square map.
fn render_affine_background(
    bus: &MemoryBus,
    bg: u32,
    control: u16,
    palette: &[u32; 256],
    brightness_effect: Option<(bool, u16)>,
    windows: &WindowConfig,
    frame: &mut VideoFrame,
) {
    let register_base = 0x0400_0020 + (bg - 2) * 0x10;
    let pa = i32::from(bus.read16(register_base) as i16);
    let pb = i32::from(bus.read16(register_base + 2) as i16);
    let pc = i32::from(bus.read16(register_base + 4) as i16);
    let pd = i32::from(bus.read16(register_base + 6) as i16);
    let reference_x = sign_extend_28(
        u32::from(bus.read16(register_base + 8))
            | (u32::from(bus.read16(register_base + 10) & 0x0FFF) << 16),
    );
    let reference_y = sign_extend_28(
        u32::from(bus.read16(register_base + 12))
            | (u32::from(bus.read16(register_base + 14) & 0x0FFF) << 16),
    );
    let char_base = u32::from((control >> 2) & 3) * 0x4000;
    let screen_base = u32::from((control >> 8) & 0x1F) * 0x800;
    let size = 128_u32 << ((control >> 14) & 3);
    let wrap = control & (1 << 13) != 0;
    let tiles_wide = size / 8;
    // Affine maps store one byte per tile in a contiguous square. Cache the
    // map once per render rather than reading the same map entry for each of
    // its 64 output pixels.
    let mut tile_map = vec![0_u8; (tiles_wide * tiles_wide) as usize];
    for tile_y in 0..tiles_wide {
        for tile_x in 0..tiles_wide {
            let map_address = VRAM_BASE + screen_base + tile_y * tiles_wide + tile_x;
            tile_map[(tile_y * tiles_wide + tile_x) as usize] = bus.read8(map_address);
        }
    }
    let mosaic_enabled = control & (1 << 6) != 0;
    let mosaic = bus.read16(0x0400_004C);
    let mosaic_width = u32::from((mosaic & 0xF) + 1);
    let mosaic_height = u32::from(((mosaic >> 4) & 0xF) + 1);
    let mut tile_pixels = vec![None::<[u8; 64]>; 256];
    for y in 0..SCREEN_HEIGHT {
        let screen_y = if mosaic_enabled {
            (y as u32 / mosaic_height) * mosaic_height
        } else {
            y as u32
        };
        let source_x_base = reference_x + pb * screen_y as i32;
        let source_y_base = reference_y + pd * screen_y as i32;
        // Walk affine coordinates across the scanline instead of multiplying
        // the PA/PC coefficients by X for every output pixel. Mosaic pixels
        // reuse the same source coordinate until the next mosaic block.
        let mut previous_screen_x = 0_i32;
        let mut source_x_fixed = source_x_base;
        let mut source_y_fixed = source_y_base;
        for x in 0..SCREEN_WIDTH {
            let screen_x = if mosaic_enabled {
                (x as u32 / mosaic_width) * mosaic_width
            } else {
                x as u32
            } as i32;
            if x != 0 && screen_x != previous_screen_x {
                let delta = screen_x - previous_screen_x;
                source_x_fixed += pa * delta;
                source_y_fixed += pc * delta;
            }
            previous_screen_x = screen_x;
            let source_x = source_x_fixed >> 8;
            let source_y = source_y_fixed >> 8;
            let (source_x, source_y) = if wrap {
                (
                    source_x.rem_euclid(size as i32) as u32,
                    source_y.rem_euclid(size as i32) as u32,
                )
            } else if source_x < 0
                || source_y < 0
                || source_x >= size as i32
                || source_y >= size as i32
            {
                continue;
            } else {
                (source_x as u32, source_y as u32)
            };

            let tile_x = source_x >> 3;
            let tile_y = source_y >> 3;
            let tile_index = usize::from(tile_map[(tile_y * tiles_wide + tile_x) as usize]);
            let pixels = tile_pixels[tile_index].get_or_insert_with(|| {
                let tile_address = VRAM_BASE + char_base + tile_index as u32 * 64;
                let mut decoded = [0_u8; 64];
                for (index, pixel) in decoded.iter_mut().enumerate() {
                    *pixel = bus.read8(tile_address + index as u32);
                }
                decoded
            });
            let pixel_offset = ((source_y & 7) * 8 + (source_x & 7)) as usize;
            let palette_index = u32::from(pixels[pixel_offset]);
            if palette_index != 0 && windows.allows(x, y, bg as u16) {
                frame.pixels[y * SCREEN_WIDTH + x] = apply_brightness_effect(
                    palette[palette_index as usize],
                    if windows.effects_allowed(x, y) {
                        brightness_effect
                    } else {
                        None
                    },
                );
            }
        }
    }
}

/// Fills the backdrop while honoring each window's color-effect permission.
fn fill_backdrop(
    frame: &mut VideoFrame,
    color: u32,
    effect: Option<(bool, u16)>,
    windows: &WindowConfig,
) {
    for y in 0..SCREEN_HEIGHT {
        for x in 0..SCREEN_WIDTH {
            frame.pixels[y * SCREEN_WIDTH + x] = apply_brightness_effect(
                color,
                if windows.effects_allowed(x, y) {
                    effect
                } else {
                    None
                },
            );
        }
    }
}

fn sign_extend_28(value: u32) -> i32 {
    ((value << 4) as i32) >> 4
}

fn palette_snapshot(bus: &MemoryBus) -> [u32; 256] {
    std::array::from_fn(|index| rgb555(bus.read16(PALETTE_BASE + (index as u32) * 2)))
}

fn obj_palette_snapshot(bus: &MemoryBus) -> [u32; 256] {
    std::array::from_fn(|index| rgb555(bus.read16(PALETTE_BASE + 0x200 + (index as u32) * 2)))
}

fn render_text_background(
    bus: &MemoryBus,
    bg: u32,
    control: u16,
    palette: &[u32; 256],
    brightness_effect: Option<(bool, u16)>,
    windows: &WindowConfig,
    frame: &mut VideoFrame,
) {
    let hofs_address = BG0HOFS + bg * 4;
    let vofs_address = BG0VOFS + bg * 4;
    let char_base = u32::from((control >> 2) & 0x3) * 0x4000;
    let screen_base = u32::from((control >> 8) & 0x1F) * 0x800;
    let color_8bpp = control & (1 << 7) != 0;
    let size = (control >> 14) & 3;
    let (map_width, map_height) = match size {
        0 => (256_u32, 256_u32),
        1 => (512, 256),
        2 => (256, 512),
        _ => (512, 512),
    };
    let scroll_x = u32::from(bus.read16(hofs_address) & 0x01FF);
    let scroll_y = u32::from(bus.read16(vofs_address) & 0x01FF);
    let mosaic_enabled = control & (1 << 6) != 0;
    let mosaic = bus.read16(0x0400_004C);
    let mosaic_width = u32::from((mosaic & 0xF) + 1);
    let mosaic_height = u32::from(((mosaic >> 4) & 0xF) + 1);
    let map_blocks_per_row = map_width / 256;
    let tiles_wide = map_width / 8;
    let tiles_high = map_height / 8;
    // Text tile-map entries do not change during this snapshot render. Cache
    // the logical map once, translating the hardware's 256x256 screen blocks
    // into a contiguous lookup table so the inner pixel loop performs no map
    // bus reads.
    let mut tile_map = vec![0_u16; (tiles_wide * tiles_high) as usize];
    for map_y in 0..tiles_high {
        for map_x in 0..tiles_wide {
            let block_x = map_x / 32;
            let block_y = map_y / 32;
            let block = block_y * map_blocks_per_row + block_x;
            let map_offset = screen_base + block * 0x800 + ((map_y % 32) * 32 + map_x % 32) * 2;
            tile_map[(map_y * tiles_wide + map_x) as usize] = bus.read16(VRAM_BASE + map_offset);
        }
    }

    // Decode character tiles lazily. Reused tiles need only be read and
    // unpacked once per frame instead of once for every displayed pixel.
    let mut tile_pixels = vec![None::<[u8; 64]>; 1024];
    for y in 0..SCREEN_HEIGHT {
        let screen_y = if mosaic_enabled {
            (y as u32 / mosaic_height) * mosaic_height
        } else {
            y as u32
        };
        let world_y = (screen_y + scroll_y) % map_height;
        let tile_y = world_y / 8;
        let in_tile_y = world_y & 7;
        for x in 0..SCREEN_WIDTH {
            let screen_x = if mosaic_enabled {
                (x as u32 / mosaic_width) * mosaic_width
            } else {
                x as u32
            };
            let world_x = (screen_x + scroll_x) % map_width;
            let tile_x = world_x / 8;
            let in_tile_x = world_x & 7;
            let entry = tile_map[(tile_y * tiles_wide + tile_x) as usize];
            let tile_index = u32::from(entry & 0x03FF);
            let pixel_x = if entry & (1 << 10) != 0 {
                7 - in_tile_x
            } else {
                in_tile_x
            };
            let pixel_y = if entry & (1 << 11) != 0 {
                7 - in_tile_y
            } else {
                in_tile_y
            };
            // Tile-map indices are expressed in 32-byte units. In 8bpp
            // mode each tile occupies 64 bytes, so bit 0 of the tile index
            // is ignored (the tile starts on an even 32-byte slot).
            let cache_index = if color_8bpp {
                (tile_index & !1) as usize
            } else {
                tile_index as usize
            };
            let pixels = tile_pixels[cache_index].get_or_insert_with(|| {
                let tile_offset = if color_8bpp {
                    (tile_index & !1) * 32
                } else {
                    tile_index * 32
                };
                let tile_address = VRAM_BASE + char_base + tile_offset;
                let mut decoded = [0_u8; 64];
                if color_8bpp {
                    for (index, pixel) in decoded.iter_mut().enumerate() {
                        *pixel = bus.read8(tile_address + index as u32);
                    }
                } else {
                    for index in 0..32 {
                        let packed = bus.read8(tile_address + index as u32);
                        decoded[index * 2] = packed & 0x0F;
                        decoded[index * 2 + 1] = packed >> 4;
                    }
                }
                decoded
            });
            let pixel_offset = (pixel_y * 8 + pixel_x) as usize;
            let raw_index = u32::from(pixels[pixel_offset]);
            let (palette_index, transparent) = if color_8bpp {
                (raw_index, raw_index == 0)
            } else {
                (
                    raw_index + u32::from((entry >> 12) & 0xF) * 16,
                    raw_index == 0,
                )
            };
            // Color index zero is transparent even when a 4bpp palette bank
            // makes its absolute palette index nonzero.
            if !transparent && windows.allows(x, y, bg as u16) {
                frame.pixels[y * SCREEN_WIDTH + x] = apply_brightness_effect(
                    palette[palette_index as usize],
                    if windows.effects_allowed(x, y) {
                        brightness_effect
                    } else {
                        None
                    },
                );
            }
        }
    }
}

/// Returns whether a layer is visible at a screen pixel under WIN0/WIN1.
/// Window coordinates use the GBA's half-open ranges and wrap when start exceeds end.
/// Layer bits are BG0..BG3=0..3 and OBJ=4; window bit 5 controls color effects.
#[derive(Clone, Debug)]
struct WindowConfig {
    enabled: bool,
    obj_window_enabled: bool,
    obj_window_control: u16,
    obj_window_mask: Vec<bool>,
    win0_enabled: bool,
    win1_enabled: bool,
    win0_x: (usize, usize),
    win0_y: (usize, usize),
    win1_x: (usize, usize),
    win1_y: (usize, usize),
    win0_control: u16,
    win1_control: u16,
    outside_control: u16,
}

impl WindowConfig {
    fn from_bus(bus: &MemoryBus) -> Self {
        let display_control = bus.read16(0x0400_0000);
        let win0_enabled = display_control & (1 << 13) != 0;
        let win1_enabled = display_control & (1 << 14) != 0;
        let obj_window_enabled = display_control & (1 << 15) != 0;
        let win0h = bus.read16(0x0400_0040);
        let win1h = bus.read16(0x0400_0042);
        let win0v = bus.read16(0x0400_0044);
        let win1v = bus.read16(0x0400_0046);
        Self {
            enabled: win0_enabled || win1_enabled || obj_window_enabled,
            obj_window_enabled,
            obj_window_control: bus.read16(0x0400_004A) >> 8 & 0x003F,
            obj_window_mask: vec![false; SCREEN_WIDTH * SCREEN_HEIGHT],
            win0_enabled,
            win1_enabled,
            win0_x: (usize::from(win0h & 0xFF), usize::from(win0h >> 8)),
            win0_y: (usize::from(win0v & 0xFF), usize::from(win0v >> 8)),
            win1_x: (usize::from(win1h & 0xFF), usize::from(win1h >> 8)),
            win1_y: (usize::from(win1v & 0xFF), usize::from(win1v >> 8)),
            win0_control: bus.read16(0x0400_0048) & 0x003F,
            win1_control: bus.read16(0x0400_0048) >> 8 & 0x003F,
            outside_control: bus.read16(0x0400_004A) & 0x003F,
        }
    }

    fn inside(coordinate: usize, (start, end): (usize, usize)) -> bool {
        if start <= end {
            coordinate >= start && coordinate < end
        } else {
            coordinate >= start || coordinate < end
        }
    }

    fn control_at(&self, x: usize, y: usize) -> u16 {
        if !self.enabled {
            // With no windows enabled, layer visibility and color effects are unrestricted.
            return 0x3F;
        }
        if self.win0_enabled && Self::inside(x, self.win0_x) && Self::inside(y, self.win0_y) {
            self.win0_control
        } else if self.win1_enabled && Self::inside(x, self.win1_x) && Self::inside(y, self.win1_y)
        {
            self.win1_control
        } else if self.obj_window_enabled && self.obj_window_mask[y * SCREEN_WIDTH + x] {
            self.obj_window_control
        } else {
            self.outside_control
        }
    }

    fn allows(&self, x: usize, y: usize, layer: u16) -> bool {
        self.control_at(x, y) & (1 << layer) != 0
    }

    /// Window control bit 5 independently gates color effects at each pixel.
    fn effects_allowed(&self, x: usize, y: usize) -> bool {
        self.control_at(x, y) & (1 << 5) != 0
    }
}

/// Reads a little-endian halfword from the snapshotted OAM address window.
fn read_oam16(snapshot: &[u8; 1024], address: u32) -> u16 {
    let offset = (address.wrapping_sub(0x0700_0000) as usize) & 0x3FF;
    u16::from_le_bytes([snapshot[offset], snapshot[(offset + 1) & 0x3FF]])
}

/// Selects the first 32 eligible OAM entries intersecting each visible scanline.
/// The result is computed once per frame and shared across OBJ priority passes.
fn object_scanline_mask(oam_snapshot: &[u8; 1024]) -> [[bool; SCREEN_HEIGHT]; 128] {
    const OAM_BASE: u32 = 0x0700_0000;
    let mut allowed_lines = [[false; SCREEN_HEIGHT]; 128];
    let mut objects_per_line = [0_u8; SCREEN_HEIGHT];
    for index in 0..128_u32 {
        let base = OAM_BASE + index * 8;
        let attr0 = read_oam16(oam_snapshot, base);
        let attr1 = read_oam16(oam_snapshot, base + 2);
        let affine = attr0 & (1 << 8) != 0;
        let object_mode = (attr0 >> 10) & 3;
        // OBJ-window entries still consume per-scanline object evaluation; only the
        // prohibited object mode is excluded from the hardware's 32-object limit.
        if (!affine && attr0 & (1 << 9) != 0) || object_mode == 3 || (attr0 >> 14) & 3 == 3 {
            continue;
        }
        let shape = (attr0 >> 14) & 3;
        let size = (attr1 >> 14) & 3;
        let (_, height) = match (shape, size) {
            (0, 0) => (8_u32, 8_u32),
            (0, 1) => (16, 16),
            (0, 2) => (32, 32),
            (0, 3) => (64, 64),
            (1, 0) => (16, 8),
            (1, 1) => (32, 8),
            (1, 2) => (32, 16),
            (1, 3) => (64, 32),
            (2, 0) => (8, 16),
            (2, 1) => (8, 32),
            (2, 2) => (16, 32),
            (2, 3) => (32, 64),
            _ => continue,
        };
        let bounding_height = if affine && attr0 & (1 << 9) != 0 {
            height * 2
        } else {
            height
        };
        let raw_y = u32::from(attr0 & 0x00FF);
        let origin_y = if raw_y >= 256 - bounding_height {
            raw_y as i32 - 256
        } else {
            raw_y as i32
        };
        for dy in 0..bounding_height {
            let screen_y = origin_y + dy as i32;
            if !(0..SCREEN_HEIGHT as i32).contains(&screen_y) {
                continue;
            }
            let line = screen_y as usize;
            if objects_per_line[line] < 32 {
                allowed_lines[index as usize][line] = true;
                objects_per_line[line] += 1;
            }
        }
    }

    allowed_lines
}

/// Returns EVA/EVB when bitmap modes show the backdrop instead of BG2 and
/// the backdrop is selected as BLDCNT's second alpha-blending target.
fn bitmap_backdrop_alpha_blend(bus: &MemoryBus) -> Option<(u16, u16)> {
    let blend_control = bus.read16(0x0400_0050);
    if (blend_control >> 6) & 3 != 1 || blend_control & (1 << 13) == 0 {
        return None;
    }
    let coefficients = bus.read16(0x0400_0052);
    Some((
        (coefficients & 0x1F).min(16),
        ((coefficients >> 8) & 0x1F).min(16),
    ))
}

/// Returns EVA/EVB for bitmap BG2 when it is enabled as a second blend target.
fn bitmap_alpha_blend(bus: &MemoryBus, display_control: u16) -> Option<(u16, u16)> {
    if display_control & (1 << 10) == 0 {
        return None;
    }
    let blend_control = bus.read16(0x0400_0050);
    // BLDCNT bits 6-7 select the effect; semi-transparent OBJ pixels only
    // alpha-blend when that effect is enabled. BG2 is target 2 at bit 10
    // (bits 8-11 select BG0-BG3 respectively).
    if (blend_control >> 6) & 3 != 1 || blend_control & (1 << 10) == 0 {
        return None;
    }
    let coefficients = bus.read16(0x0400_0052);
    Some((
        (coefficients & 0x1F).min(16),
        ((coefficients >> 8) & 0x1F).min(16),
    ))
}

/// Returns BG2 brightness/darkness settings for bitmap modes when BG2 is a first target.
/// Alpha blending is handled separately because it needs the second compositing target.
fn bitmap_brightness_effect(
    bus: &MemoryBus,
    display_control: u16,
    target_bit: u16,
) -> Option<(bool, u16)> {
    let control = bus.read16(0x0400_0050);
    if control & (1 << target_bit) == 0 {
        return None;
    }
    let brighten = match (control >> 6) & 3 {
        2 => true,
        3 => false,
        _ => return None,
    };
    Some((brighten, (bus.read16(0x0400_0054) & 0x1F).min(16)))
}

/// Applies GBA BLDY brightness or darkness to an opaque RGB555-derived color.
fn apply_brightness_effect(color: u32, effect: Option<(bool, u16)>) -> u32 {
    let Some((brighten, coefficient)) = effect else {
        return color;
    };
    let transform = |shift: u32| {
        let channel = ((color >> shift) & 0xFF) >> 3;
        let amount = u32::from(coefficient);
        let result = if brighten {
            channel + (((31 - channel) * amount) >> 4)
        } else {
            channel - ((channel * amount) >> 4)
        };
        (result.min(31) << 3) | (result.min(31) >> 2)
    };
    0xFF00_0000 | (transform(16) << 16) | (transform(8) << 8) | transform(0)
}

fn blend_argb555(source: u32, target: u32, eva: u16, evb: u16) -> u32 {
    let blend_channel = |shift: u32| {
        let source = (source >> shift) & 0xFF;
        let target = (target >> shift) & 0xFF;
        let source5 = source >> 3;
        let target5 = target >> 3;
        let value = ((source5 * u32::from(eva) + target5 * u32::from(evb)) >> 4).min(31);
        (value << 3) | (value >> 2)
    };
    0xFF00_0000 | (blend_channel(16) << 16) | (blend_channel(8) << 8) | blend_channel(0)
}

/// Draws regular (non-affine) OBJ sprites for the requested priority.
/// OBJ is composited after backgrounds of the same priority; lower OAM indices
/// win ties, so entries are visited from 127 down to 0.
fn render_regular_objects(
    bus: &MemoryBus,
    display_control: u16,
    priority: u16,
    palette: &[u32; 256],
    tile_cache: &mut [Option<[u8; 64]>],
    oam_snapshot: &[u8; 1024],
    allowed_lines: &[[bool; SCREEN_HEIGHT]; 128],
    bitmap_blend: Option<(u16, u16)>,
    bitmap_obj_effect: Option<(bool, u16)>,
    mut obj_window_mask: Option<&mut [bool]>,
    windows: &WindowConfig,
    frame: &mut VideoFrame,
) {
    if display_control & (1 << 12) == 0 {
        return;
    }
    const OAM_BASE: u32 = 0x0700_0000;
    let mode = display_control & 7;
    // OBJ tile indices remain relative to 0x06010000 in every display mode.
    // Bitmap modes reserve the first 16 KiB of OBJ VRAM, so indices below 512
    // are unavailable there; adding 0x06014000 as a new base would double-offset
    // valid bitmap-mode tile indices.
    let obj_vram_base: u32 = 0x0601_0000;
    let one_dimensional = display_control & (1 << 6) != 0;
    let mosaic = bus.read16(0x0400_004C);
    let object_mosaic_width = u32::from(((mosaic >> 8) & 0xF) + 1);
    let object_mosaic_height = u32::from(((mosaic >> 12) & 0xF) + 1);

    for index in (0..128_u32).rev() {
        let base = OAM_BASE + index * 8;
        let attr0 = read_oam16(oam_snapshot, base);
        let attr1 = read_oam16(oam_snapshot, base + 2);
        let attr2 = read_oam16(oam_snapshot, base + 4);

        // Bit 8 selects affine transformation. Bit 9 disables a regular
        // sprite, but doubles the affine sprite's destination bounding box.
        let affine = attr0 & (1 << 8) != 0;
        let object_mode = (attr0 >> 10) & 3;
        // OBJ-window entries and prohibited OBJ modes do not draw regular
        // sprite pixels. Semi-transparent OBJ mode (1) still draws its source
        // pixels; alpha blending is handled by the compositor when available.
        if (!affine && attr0 & (1 << 9) != 0) || object_mode == 3 || (attr0 >> 14) & 3 == 3 {
            continue;
        }
        if ((obj_window_mask.is_some() && object_mode != 2)
            || (obj_window_mask.is_none() && object_mode == 2)
            || ((attr2 >> 10) & 3) != priority)
        {
            continue;
        }

        let shape = (attr0 >> 14) & 3;
        let size = (attr1 >> 14) & 3;
        let (width, height) = match (shape, size) {
            (0, 0) => (8_u32, 8_u32),
            (0, 1) => (16, 16),
            (0, 2) => (32, 32),
            (0, 3) => (64, 64),
            (1, 0) => (16, 8),
            (1, 1) => (32, 8),
            (1, 2) => (32, 16),
            (1, 3) => (64, 32),
            (2, 0) => (8, 16),
            (2, 1) => (8, 32),
            (2, 2) => (16, 32),
            (2, 3) => (32, 64),
            _ => continue,
        };
        let bounding_width = if affine && attr0 & (1 << 9) != 0 {
            width * 2
        } else {
            width
        };
        let bounding_height = if affine && attr0 & (1 << 9) != 0 {
            height * 2
        } else {
            height
        };

        let raw_x = u32::from(attr1 & 0x01FF);
        let origin_x = if raw_x >= 256 {
            raw_x as i32 - 512
        } else {
            raw_x as i32
        };
        let raw_y = u32::from(attr0 & 0x00FF);
        let origin_y = if raw_y >= 256 - bounding_height {
            raw_y as i32 - 256
        } else {
            raw_y as i32
        };
        let color_8bpp = attr0 & (1 << 13) != 0;
        let raw_tile_index = u32::from(attr2 & 0x03FF);
        // In bitmap modes OBJ VRAM starts at tile index 512 (0x06014000).
        // Lower indices overlap the framebuffer and cannot be used as OBJ data.
        if mode >= 3 && raw_tile_index < 512 {
            continue;
        }
        let tile_index = raw_tile_index & if color_8bpp { !1 } else { !0 };
        let palette_bank = u32::from((attr2 >> 12) & 0xF);
        let matrix_index = u32::from((attr1 >> 9) & 0x1F);
        let matrix_address = OAM_BASE + matrix_index * 32 + 6;
        let (pa, pb, pc, pd) = if affine {
            (
                i32::from(read_oam16(oam_snapshot, matrix_address) as i16),
                i32::from(read_oam16(oam_snapshot, matrix_address + 8) as i16),
                i32::from(read_oam16(oam_snapshot, matrix_address + 16) as i16),
                i32::from(read_oam16(oam_snapshot, matrix_address + 24) as i16),
            )
        } else {
            (0x100, 0, 0, 0x100)
        };
        let hflip = !affine && attr1 & (1 << 12) != 0;
        let vflip = !affine && attr1 & (1 << 13) != 0;
        let mosaic_enabled = attr0 & (1 << 12) != 0;
        let row_stride = if one_dimensional {
            (width / 8) * if color_8bpp { 2 } else { 1 }
        } else {
            32
        };

        for dy in 0..bounding_height {
            let screen_y = origin_y + dy as i32;
            if !(0..SCREEN_HEIGHT as i32).contains(&screen_y) {
                continue;
            }
            if !allowed_lines[index as usize][screen_y as usize] {
                continue;
            }
            let sampled_y = if mosaic_enabled {
                (dy / object_mosaic_height) * object_mosaic_height
            } else {
                dy
            };
            for dx in 0..bounding_width {
                let screen_x = origin_x + dx as i32;
                if !(0..SCREEN_WIDTH as i32).contains(&screen_x) {
                    continue;
                }
                let sampled_x = if mosaic_enabled {
                    (dx / object_mosaic_width) * object_mosaic_width
                } else {
                    dx
                };
                let (source_x, source_y) = if affine {
                    let centered_x = sampled_x as i32 - bounding_width as i32 / 2;
                    let centered_y = sampled_y as i32 - bounding_height as i32 / 2;
                    (
                        ((pa * centered_x + pb * centered_y) >> 8) + width as i32 / 2,
                        ((pc * centered_x + pd * centered_y) >> 8) + height as i32 / 2,
                    )
                } else {
                    (
                        (if hflip {
                            width - 1 - sampled_x
                        } else {
                            sampled_x
                        }) as i32,
                        (if vflip {
                            height - 1 - sampled_y
                        } else {
                            sampled_y
                        }) as i32,
                    )
                };
                if source_x < 0
                    || source_y < 0
                    || source_x >= width as i32
                    || source_y >= height as i32
                {
                    continue;
                }
                let source_x = source_x as u32;
                let source_y = source_y as u32;
                let tile_x = source_x / 8;
                let tile_y = source_y / 8;
                let slot =
                    tile_index + tile_y * row_stride + tile_x * if color_8bpp { 2 } else { 1 };
                // OBJ VRAM mirrors after its 32 KiB character-data window.
                // Keep the cache index in that physical range as well.
                let slot = slot & 0x03FF;
                let cache_index = if color_8bpp { slot & !1 } else { slot } as usize;
                let pixels = tile_cache[cache_index].get_or_insert_with(|| {
                    let tile_address = obj_vram_base + cache_index as u32 * 32;
                    let mut decoded = [0_u8; 64];
                    if color_8bpp {
                        for (pixel_index, pixel) in decoded.iter_mut().enumerate() {
                            *pixel = bus.read8(tile_address + pixel_index as u32);
                        }
                    } else {
                        for byte_index in 0..32 {
                            let packed = bus.read8(tile_address + byte_index as u32);
                            decoded[byte_index * 2] = packed & 0x0F;
                            decoded[byte_index * 2 + 1] = packed >> 4;
                        }
                    }
                    decoded
                });
                let raw_index = u32::from(pixels[((source_y & 7) * 8 + (source_x & 7)) as usize]);
                let palette_index = if color_8bpp {
                    raw_index
                } else {
                    if raw_index == 0 {
                        continue;
                    }
                    palette_bank * 16 + raw_index
                };
                if palette_index == 0 {
                    continue;
                }
                if object_mode == 2 {
                    if let Some(mask) = obj_window_mask.as_deref_mut() {
                        mask[screen_y as usize * SCREEN_WIDTH + screen_x as usize] = true;
                    }
                    continue;
                }
                // OBJ palette RAM is stable for this snapshot render. Reuse
                // the decoded palette rather than performing a bus read and
                // RGB555 conversion for every visible sprite pixel.
                let destination = screen_y as usize * SCREEN_WIDTH + screen_x as usize;
                if !windows.allows(screen_x as usize, screen_y as usize, 4) {
                    continue;
                }
                let source_color = palette[palette_index as usize];
                frame.pixels[destination] = if object_mode == 1 {
                    // Semi-transparent OBJ pixels are forced first targets for
                    // alpha blending. If alpha blending is unavailable or
                    // masked by the current window, they render normally; they
                    // must not fall through to ordinary OBJ brighten/darken.
                    if mode >= 3 && windows.effects_allowed(screen_x as usize, screen_y as usize) {
                        if let Some((eva, evb)) = bitmap_blend {
                            blend_argb555(source_color, frame.pixels[destination], eva, evb)
                        } else {
                            source_color
                        }
                    } else {
                        source_color
                    }
                } else {
                    apply_brightness_effect(
                        source_color,
                        if windows.effects_allowed(screen_x as usize, screen_y as usize) {
                            bitmap_obj_effect
                        } else {
                            None
                        },
                    )
                };
            }
        }
    }
}

/// Maps a screen pixel through BG2's affine matrix into bitmap VRAM.
/// Cached affine transform for one bitmap frame. Affine registers are stable
/// during this snapshot render, so read them once instead of once per pixel.
struct BitmapTransform {
    width: usize,
    height: usize,
    mosaic_enabled: bool,
    mosaic_width: usize,
    mosaic_height: usize,
    wrap: bool,
    pa: i32,
    pb: i32,
    pc: i32,
    pd: i32,
    reference_x: i32,
    reference_y: i32,
}

impl BitmapTransform {
    fn new(
        bus: &MemoryBus,
        width: usize,
        height: usize,
        mosaic_enabled: bool,
        mosaic_width: usize,
        mosaic_height: usize,
        wrap: bool,
    ) -> Self {
        Self {
            width,
            height,
            mosaic_enabled,
            mosaic_width,
            mosaic_height,
            wrap,
            pa: i32::from(bus.read16(0x0400_0020) as i16),
            pb: i32::from(bus.read16(0x0400_0022) as i16),
            pc: i32::from(bus.read16(0x0400_0024) as i16),
            pd: i32::from(bus.read16(0x0400_0026) as i16),
            reference_x: sign_extend_28(
                u32::from(bus.read16(0x0400_0028))
                    | (u32::from(bus.read16(0x0400_002A) & 0x0FFF) << 16),
            ),
            reference_y: sign_extend_28(
                u32::from(bus.read16(0x0400_002C))
                    | (u32::from(bus.read16(0x0400_002E) & 0x0FFF) << 16),
            ),
        }
    }

    fn source_coordinates(&self, x: usize, y: usize) -> Option<(usize, usize)> {
        let x = if self.mosaic_enabled {
            x / self.mosaic_width * self.mosaic_width
        } else {
            x
        };
        let y = if self.mosaic_enabled {
            y / self.mosaic_height * self.mosaic_height
        } else {
            y
        };
        let source_x = (self.reference_x + self.pa * x as i32 + self.pb * y as i32) >> 8;
        let source_y = (self.reference_y + self.pc * x as i32 + self.pd * y as i32) >> 8;
        if self.wrap {
            Some((
                source_x.rem_euclid(self.width as i32) as usize,
                source_y.rem_euclid(self.height as i32) as usize,
            ))
        } else if source_x < 0
            || source_y < 0
            || source_x >= self.width as i32
            || source_y >= self.height as i32
        {
            None
        } else {
            Some((source_x as usize, source_y as usize))
        }
    }
}

fn rgb555(color: u16) -> u32 {
    let red = u32::from(color & 0x1F);
    let green = u32::from((color >> 5) & 0x1F);
    let blue = u32::from((color >> 10) & 0x1F);
    let red = (red << 3) | (red >> 2);
    let green = (green << 3) | (green >> 2);
    let blue = (blue << 3) | (blue >> 2);
    0xFF00_0000 | (red << 16) | (green << 8) | blue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GbaCartridge, MemoryBus};

    #[test]
    fn video_frame_validates_public_dimensions_and_handles_short_buffers() {
        let valid = VideoFrame {
            width: 2,
            height: 2,
            pixels: vec![1, 2, 3, 4],
        };
        assert!(valid.is_well_formed());
        assert_eq!(valid.pixel(1, 1), Some(4));
        assert_eq!(valid.row(0), Some(&[1, 2][..]));
        assert_eq!(valid.row(2), None);

        let mut editable = valid.clone();
        assert!(editable.set_pixel(0, 1, 30));
        assert_eq!(editable.row(1), Some(&[30, 4][..]));
        if let Some(row) = editable.row_mut(0) {
            row[1] = 20;
        }
        assert_eq!(editable.pixel(1, 0), Some(20));
        assert!(!editable.set_pixel(2, 0, 99));

        let short_buffer = VideoFrame {
            width: 2,
            height: 2,
            pixels: vec![1, 2, 3],
        };
        assert!(!short_buffer.is_well_formed());
        assert_eq!(short_buffer.pixel(1, 1), None);

        let overflowing_dimensions = VideoFrame {
            width: usize::MAX,
            height: 2,
            pixels: Vec::new(),
        };
        assert!(!overflowing_dimensions.is_well_formed());
        assert_eq!(overflowing_dimensions.pixel(0, 1), None);
    }

    #[test]
    fn consuming_frame_exports_pixels_without_changing_pixel_order() {
        let frame = VideoFrame {
            width: 2,
            height: 2,
            pixels: vec![0xFF00_0001, 0xFF00_0002, 0xFF00_0003, 0xFF00_0004],
        };
        assert_eq!(
            frame.into_pixels(),
            vec![0xFF00_0001, 0xFF00_0002, 0xFF00_0003, 0xFF00_0004]
        );
    }

    #[test]
    fn video_frame_exports_rgb888_and_rgba8888_in_channel_order() {
        let frame = VideoFrame {
            width: 2,
            height: 1,
            pixels: vec![0xFF12_3456, 0xFFAB_CDEF],
        };
        assert_eq!(frame.to_rgb888(), Some(vec![0x12, 0x34, 0x56, 0xAB, 0xCD, 0xEF]));
        assert_eq!(
            frame.to_rgba8888(),
            Some(vec![0x12, 0x34, 0x56, 0xFF, 0xAB, 0xCD, 0xEF, 0xFF])
        );
    }

    #[test]
    fn video_frame_export_rejects_incomplete_buffers() {
        let malformed = VideoFrame {
            width: 2,
            height: 1,
            pixels: vec![0xFF12_3456],
        };
        assert_eq!(malformed.to_rgb888(), None);
        assert_eq!(malformed.to_rgba8888(), None);
    }

    #[test]
    fn video_frame_exports_bgra8888_and_rgb565_little_endian() {
        let frame = VideoFrame {
            width: 1,
            height: 1,
            pixels: vec![0xFF12_3456],
        };
        assert_eq!(frame.to_bgra8888(), Some(vec![0x56, 0x34, 0x12, 0xFF]));
        // 0x12_3456 -> R=2, G=13, B=10 at RGB565 precision -> 0x11AA.
        assert_eq!(frame.to_rgb565_le(), Some(vec![0xAA, 0x11]));
    }

    fn bus() -> MemoryBus {
        let mut rom = vec![0; 0xC0];
        rom[0xBD] = rom[0xA0..=0xBC]
            .iter()
            .fold(0x19_u8, |sum, byte| sum.wrapping_add(*byte))
            .wrapping_neg();
        let mut bus = MemoryBus::new(GbaCartridge::parse(&rom).expect("valid test ROM"));
        // Keep bitmap-mode tests on the default untransformed BG2 plane unless
        // a test explicitly programs a different affine matrix.
        bus.write16(0x0400_0020, 0x0100); // BG2 PA = 1.0
        bus.write16(0x0400_0026, 0x0100); // BG2 PD = 1.0
                                          // Real software initializes unused OAM entries as disabled. Keeping
                                          // the test bus deterministic prevents default all-zero entries from
                                          // aliasing tile 0 and overwriting the sprite under test.
        for index in 0..128_u32 {
            bus.write16(0x0700_0000 + index * 8, 1 << 9);
        }
        bus
    }

    #[test]
    fn bitmap_modes_show_palette_backdrop_when_bg2_is_disabled() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3); // Mode 3, BG2 intentionally disabled.
        bus.write16(PALETTE_BASE, 0x03E0); // backdrop green
        bus.write16(VRAM_BASE, 0x001F); // framebuffer pixel is red but must be hidden.

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_FF00));
        assert_eq!(frame.pixel(239, 159), Some(0xFF00_FF00));
    }

    #[test]
    fn mode2_renders_affine_bg3_from_its_own_registers_and_map() {
        let mut bus = bus();
        // Mode 2 with BG3 enabled. Keep its affine map separate from character data.
        bus.write16(0x0400_0000, 2 | (1 << 11));
        bus.write16(0x0400_000E, (1 << 2) | (9 << 8));
        bus.write16(VRAM_BASE + 0x4800, 1);
        bus.write16(VRAM_BASE + 0x4000, 1);
        bus.write16(VRAM_BASE + 0x4000 + 64, 1);
        bus.write16(PALETTE_BASE + 2, 0x03E0); // palette 1: green
        bus.write16(0x0400_0030, 0x0100); // BG3 PA
        bus.write16(0x0400_0036, 0x0100); // BG3 PD

        let frame = render_bitmap_frame(&bus).expect("mode 2 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_FF00));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_0000));
    }

    #[test]
    fn regular_obj_mosaic_repeats_pixels_and_rows() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12); // OBJ enabled
        bus.write16(0x0400_004C, (1 << 8) | (1 << 12)); // OBJ mosaic: 2x2
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ palette 1: red
        bus.write16(PALETTE_BASE + 0x204, 0x03E0); // OBJ palette 2: green
        bus.write16(PALETTE_BASE + 0x206, 0x03E0); // OBJ palette 3: green
        bus.write16(VRAM_BASE + 0x10000, 0x2321); // pixels 1,2,3,2
        bus.write16(0x0700_0000, 1 << 12); // Mosaic, 4bpp, 8x8 OBJ
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(0, 1), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(2, 0), Some(0xFF00_FF00));
    }

    #[test]
    fn bitmap_mode3_uses_bg2_affine_matrix() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10));
        bus.write16(0x0400_0020, 0x0100);
        bus.write16(0x0400_0026, 0x0100);
        bus.write16(VRAM_BASE, 0x001F);
        bus.write16(VRAM_BASE + 2, 0x03E0);
        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_FF00));
    }

    #[test]
    fn bitmap_mode4_uses_affine_coordinates_and_selected_page() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 4 | (1 << 4) | (1 << 10));
        // VRAM byte stores replicate the value across the addressed halfword.
        bus.write16(VRAM_BASE + 0xA000, 0x0201);
        bus.write16(PALETTE_BASE + 2, 0x001F);
        bus.write16(PALETTE_BASE + 4, 0x03E0);
        bus.write16(0x0400_0020, 0x0200); // PA = 2.0, skip every other source pixel

        let frame = render_bitmap_frame(&bus).expect("mode 4 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_0000));
    }

    #[test]
    fn bitmap_mode5_affine_reference_moves_centered_viewport_source() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 5 | (1 << 10));
        bus.write16(VRAM_BASE, 0x001F);
        bus.write16(VRAM_BASE + 2, 0x03E0);
        bus.write16(VRAM_BASE + 4, 0x7C00);
        bus.write16(0x0400_0028, 0x0100); // Start from source x = 1

        let frame = render_bitmap_frame(&bus).expect("mode 5 should render");
        assert_eq!(frame.pixel(40, 16), Some(0xFF00_FF00));
        assert_eq!(frame.pixel(41, 16), Some(0xFF00_00FF));
    }

    #[test]
    fn bitmap_mode5_affine_reference_wraps_when_overflow_is_enabled() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 5 | (1 << 10));
        bus.write16(0x0400_000C, 1 << 13); // BG2 affine overflow/wrap.
        bus.write16(0x0400_0028, 0xFFFF);
        bus.write16(0x0400_002A, 0x0FFF); // source x starts at -1
        bus.write16(VRAM_BASE + ((MODE_5_WIDTH - 1) * 2) as u32, 0x001F);
        bus.write16(VRAM_BASE, 0x03E0);

        let frame = render_bitmap_frame(&bus).expect("mode 5 should render");
        assert_eq!(frame.pixel(40, 16), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(41, 16), Some(0xFF00_FF00));
    }

    #[test]
    fn bitmap_mode3_affine_reference_wraps_when_overflow_is_enabled() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10));
        bus.write16(0x0400_000C, 1 << 13); // BG2 affine overflow/wrap.
        bus.write16(0x0400_0028, 0xFFFF);
        bus.write16(0x0400_002A, 0x0FFF); // source x starts at -1
        bus.write16(VRAM_BASE + ((SCREEN_WIDTH - 1) * 2) as u32, 0x001F);
        bus.write16(VRAM_BASE, 0x03E0);
        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_FF00));
    }

    #[test]
    fn bitmap_mode3_affine_reference_clips_without_overflow_wrap() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10));
        bus.write16(PALETTE_BASE, 0x7C00); // blue backdrop
        bus.write16(0x0400_0020, 0x0100);
        bus.write16(0x0400_0028, 0xFFFF); // source x starts at -1
        bus.write16(0x0400_002A, 0x0FFF);
        bus.write16(VRAM_BASE, 0x001F);
        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_00FF));
        assert_eq!(frame.pixel(1, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn bitmap_mode3_applies_bg2_mosaic_to_source_pixels() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10)); // Mode 3, BG2 enabled
        bus.write16(BG0CNT + 4, 1 << 6); // BG2 mosaic
        bus.write16(0x0400_004C, 1 | (1 << 4)); // 2x2 mosaic
        bus.write16(VRAM_BASE, 0x001F); // red at (0,0)
        bus.write16(VRAM_BASE + 2, 0x03E0); // green at (1,0)
        bus.write16(VRAM_BASE + 4, 0x03E0); // green at (2,0)
        bus.write16(VRAM_BASE + (SCREEN_WIDTH * 2) as u32, 0x7C00); // blue at (0,1)

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(0, 1), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(2, 0), Some(0xFF00_FF00));
    }

    #[test]
    fn bitmap_mode4_applies_bg2_mosaic_to_palette_indices() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 4 | (1 << 10));
        bus.write16(BG0CNT + 4, 1 << 6);
        bus.write16(0x0400_004C, 1 | (1 << 4));
        bus.write16(VRAM_BASE, 0x0201); // index 1 then index 2
        bus.write16(VRAM_BASE + 2, 0x0002); // index 2 at pixel 2
        bus.write16(PALETTE_BASE + 2, 0x001F); // red
        bus.write16(PALETTE_BASE + 4, 0x03E0); // green

        let frame = render_bitmap_frame(&bus).expect("mode 4 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(0, 1), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(2, 0), Some(0xFF00_FF00));
    }

    #[test]
    fn bitmap_mode5_applies_bg2_mosaic_inside_centered_viewport() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 5 | (1 << 10));
        bus.write16(BG0CNT + 4, 1 << 6);
        bus.write16(0x0400_004C, 1 | (1 << 4));
        bus.write16(VRAM_BASE, 0x001F); // red at local (0,0)
        bus.write16(VRAM_BASE + 2, 0x03E0); // green at local (1,0)

        let frame = render_bitmap_frame(&bus).expect("mode 5 should render");
        assert_eq!(frame.pixel(40, 16), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(41, 16), Some(0xFFFF_0000));
    }

    #[test]
    fn mode5_uses_backdrop_color_outside_centered_bitmap() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 5 | (1 << 10)); // Mode 5, BG2 enabled
        bus.write16(PALETTE_BASE, 0x7C00); // Blue backdrop
        bus.write16(VRAM_BASE, 0x001F); // Red first bitmap pixel

        let frame = render_bitmap_frame(&bus).expect("mode 5 should render");
        assert_eq!(frame.pixel(39, 15), Some(0xFF00_00FF)); // Outside viewport
        assert_eq!(frame.pixel(40, 16), Some(0xFFFF_0000)); // Viewport origin
        assert_eq!(frame.pixel(199, 143), Some(0xFF00_0000)); // Last pixel inside viewport, still zero-filled
    }

    #[test]
    fn mode2_ignores_text_background_enable_bits() {
        let mut bus = bus();
        // BG0 is unavailable in mode 2, even when DISPCNT bit 8 is set.
        bus.write16(0x0400_0000, 2 | (1 << 8));
        bus.write16(BG0CNT, 0);
        bus.write16(VRAM_BASE, 1);
        bus.write16(VRAM_BASE + 0x4000, 1);
        bus.write16(PALETTE_BASE + 2, 0x001F); // palette 1: red

        let frame = render_bitmap_frame(&bus).expect("mode 2 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_0000));
    }

    #[test]
    fn semi_transparent_obj_mode_keeps_sprite_visible_without_blending() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12); // OBJ enabled
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ index 1: red
        bus.write16(0x0601_0000, 0x1111);
        bus.write16(0x0700_0000, 1 << 10); // semi-transparent 8x8 OBJ
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        // Full alpha-effect composition is not implemented yet, but mode 1
        // must not make the OBJ disappear entirely.
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn affine_background_mosaic_repeats_source_pixels_and_rows() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 2 | (1 << 10)); // Mode 2, BG2 enabled
        bus.write16(BG0CNT + 4, (1 << 6) | (1 << 13)); // BG2 mosaic and wrap
        bus.write16(0x0400_0020, 0x0100); // PA = 1.0
        bus.write16(0x0400_0026, 0x0100); // PD = 1.0
        bus.write16(0x0400_004C, 1 | (1 << 4)); // 2x2 mosaic
        bus.write8(VRAM_BASE, 0);
        bus.write8(VRAM_BASE + 1, 1);
        bus.write8(VRAM_BASE + 8, 2);
        bus.write16(PALETTE_BASE + 2, 0x001F);
        bus.write16(PALETTE_BASE + 4, 0x03E0);

        let frame = render_bitmap_frame(&bus).unwrap();
        assert_eq!(frame.pixel(0, 0), frame.pixel(1, 0));
        assert_eq!(frame.pixel(0, 0), frame.pixel(0, 1));
        assert_eq!(frame.pixel(2, 0), frame.pixel(2, 1));
    }

    #[test]
    fn affine_background_without_wrap_clips_outside_map() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 2 | (1 << 10)); // mode 2, BG2 enabled
        bus.write16(0x0400_000C, 0);
        bus.write16(VRAM_BASE, 1);
        bus.write16(VRAM_BASE + 64, 1);
        bus.write16(PALETTE_BASE + 2, 0x001F);
        // Identity transform; reference X = -1.0 clips pixel zero but not pixel one.
        bus.write16(0x0400_0020, 0x0100);
        bus.write16(0x0400_0026, 0x0100);
        bus.write16(0x0400_0028, 0xFF00);
        bus.write16(0x0400_002A, 0x0FFF);

        let frame = render_bitmap_frame(&bus).expect("mode 2 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn affine_reference_sign_extension_handles_positive_and_negative_values() {
        assert_eq!(sign_extend_28(0x0000_1234), 0x0000_1234);
        assert_eq!(sign_extend_28(0x0FFF_FF00), -256);
        assert_eq!(sign_extend_28(0x0800_0000), -134_217_728);
    }

    #[test]
    fn mode1_renders_affine_bg2_with_identity_transform() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 | (1 << 10)); // mode 1, BG2 enabled
        bus.write16(0x0400_000C, 0); // affine map at screen block 0
        bus.write16(VRAM_BASE, 1); // map entry selects tile 1
        bus.write16(VRAM_BASE + 64, 1); // pixel 0 red; pixel 1 remains transparent
        bus.write16(PALETTE_BASE + 2, 0x001F); // palette 1: red
        bus.write16(0x0400_0020, 0x0100); // PA = 1.0
        bus.write16(0x0400_0022, 0); // PB = 0
        bus.write16(0x0400_0024, 0); // PC = 0
        bus.write16(0x0400_0026, 0x0100); // PD = 1.0

        let frame = render_bitmap_frame(&bus).expect("mode 1 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_0000));
    }

    #[test]
    fn affine_background_wraps_coordinates_when_overflow_enabled() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 2 | (1 << 10)); // mode 2, BG2 enabled
        bus.write16(0x0400_000C, 1 << 13); // wrap enabled, 128x128 map
        bus.write16(0x0400_0020, 0x0100);
        bus.write16(0x0400_0026, 0x0100);
        // Start at x=-1 pixel (signed 28-bit 8.8 fixed point).
        bus.write16(0x0400_0028, 0xFF00);
        bus.write16(0x0400_002A, 0x0FFF);
        // The wrapped coordinate is the final pixel of the first map row.
        bus.write16(VRAM_BASE + 14, 0x0100); // only map byte 15 selects tile 1
        bus.write16(VRAM_BASE + 64 + 6, 0x0100); // only tile pixel 7 is red
        bus.write16(PALETTE_BASE + 2, 0x001F);

        let frame = render_bitmap_frame(&bus).expect("mode 2 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn mode0_renders_4bpp_bg0_tile_and_palette_bank() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 8); // mode 0, BG0 enabled
                                          // Tile 1: pixels 0 and 1 are 1 and 2.
        bus.write8(VRAM_BASE + 32, 0x21);
        // Select tile 1 by changing the map entry.
        bus.write16(VRAM_BASE, (1 << 12) | 1);
        bus.write16(PALETTE_BASE + 34, 0x001F); // palette 17: red
        bus.write16(PALETTE_BASE + 36, 0x03E0); // palette 18: green
        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_FF00));
    }

    #[test]
    fn mode0_renders_8bpp_tile_pixels_and_zero_transparency() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 8); // mode 0, BG0 enabled
        bus.write16(BG0CNT, 1 << 7); // 8bpp tiles
                                     // In 8bpp mode tile index 1 aliases tile index 0; tile 2
                                     // starts at byte offset 64 in character data.
        bus.write16(VRAM_BASE, 2);
        // VRAM byte writes replicate across the containing halfword; use a
        // halfword write to preserve distinct adjacent 8bpp palette indices.
        bus.write16(VRAM_BASE + 64, 0x0403);
        bus.write16(PALETTE_BASE, 0x7C00); // blue backdrop for transparent index zero
        bus.write16(PALETTE_BASE + 6, 0x001F); // palette 3: red
        bus.write16(PALETTE_BASE + 8, 0x03E0); // palette 4: green
        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_FF00));
        assert_eq!(frame.pixel(2, 0), Some(0xFF00_00FF));
    }

    #[test]
    fn mode0_8bpp_odd_tile_index_uses_even_tile_slot() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 8); // mode 0, BG0 enabled
        bus.write16(BG0CNT, 1 << 7); // 8bpp tiles
        bus.write16(VRAM_BASE, 3); // odd tile index aliases tile 2
        bus.write8(VRAM_BASE + 64, 5); // tile 2, first pixel
        bus.write8(VRAM_BASE + 96, 6); // tile 3 slot must not be used
        bus.write16(PALETTE_BASE + 10, 0x001F); // palette 5: red
        bus.write16(PALETTE_BASE + 12, 0x03E0); // palette 6: green
        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn mode0_zero_pixel_stays_transparent_in_nonzero_palette_bank() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 8); // mode 0, BG0 enabled
        bus.write16(PALETTE_BASE, 0x7C00); // blue backdrop
                                           // Tile 1 begins with zero (transparent); next pixel uses palette entry 17.
        bus.write16(VRAM_BASE, (1 << 12) | 1);
        bus.write8(VRAM_BASE + 32, 0x10);
        bus.write16(PALETTE_BASE + 34, 0x001F); // palette 17: red
        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_00FF));
        assert_eq!(frame.pixel(1, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn mode0_composites_backgrounds_by_priority() {
        let mut bus = bus();
        // Enable BG0 and BG1. BG0 has priority 0; BG1 has priority 1.
        bus.write16(0x0400_0000, (1 << 8) | (1 << 9));
        bus.write16(BG0CNT, 0);
        bus.write16(BG0CNT + 2, 1 | (1 << 8));
        // BG0 tile 1: first pixel is palette index 1 (red).
        bus.write16(VRAM_BASE, 1);
        bus.write8(VRAM_BASE + 32, 1);
        bus.write16(PALETTE_BASE + 2, 0x001F);
        // BG1 screen block 1, tile 2: first pixel is palette index 2 (green).
        bus.write16(VRAM_BASE + 0x800, 2);
        bus.write8(VRAM_BASE + 64, 2);
        bus.write16(PALETTE_BASE + 4, 0x03E0);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn mode0_bg_mosaic_repeats_pixels_and_rows() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 8); // BG0 enabled
        bus.write16(BG0CNT, 1 << 6); // BG0 mosaic enabled
        bus.write16(0x0400_004C, 1 | (1 << 4)); // 2x2 mosaic

        bus.write16(VRAM_BASE, 1);
        bus.write16(VRAM_BASE + 32, 0x4321);
        bus.write16(PALETTE_BASE + 2, 0x001F);
        bus.write16(PALETTE_BASE + 4, 0x03E0);
        bus.write16(PALETTE_BASE + 6, 0x7C00);
        bus.write16(PALETTE_BASE + 8, 0x7FFF);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(2, 0), Some(0xFF00_00FF));
        assert_eq!(frame.pixel(0, 1), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 1), Some(0xFFFF_0000));
    }

    #[test]
    fn mode0_bg0_wins_ties_against_bg1() {
        let mut bus = bus();
        bus.write16(0x0400_0000, (1 << 8) | (1 << 9));
        bus.write16(BG0CNT, 0); // BG0 priority 0
        bus.write16(BG0CNT + 2, 1 << 8); // BG1 priority 0, screen block 1
        bus.write16(VRAM_BASE, 1);
        bus.write8(VRAM_BASE + 32, 1);
        bus.write16(VRAM_BASE + 0x800, 2);
        bus.write8(VRAM_BASE + 64, 2);
        bus.write16(PALETTE_BASE + 2, 0x001F); // BG0 red
        bus.write16(PALETTE_BASE + 4, 0x03E0); // BG1 green

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn mode0_disabled_bg_shows_palette_backdrop() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 0);
        bus.write16(PALETTE_BASE, 0x7C00); // blue backdrop
        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_00FF));
    }

    #[test]
    fn mode0_renders_regular_4bpp_obj_over_background() {
        let mut bus = bus();
        bus.write16(0x0400_0000, (1 << 8) | (1 << 12)); // BG0 and OBJ enabled
        bus.write16(PALETTE_BASE, 0x7C00); // blue backdrop
        bus.write16(PALETTE_BASE + 2, 0x03E0); // BG palette index 1: green
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ palette index 1: red
        bus.write16(VRAM_BASE, 1);
        bus.write8(VRAM_BASE + 32, 0x11); // BG tile 1: green in both first pixels

        bus.write16(0x0601_0000, 0x0001);
        // OAM entry 0: regular 8x8 sprite at (0,0), tile 0, priority 0.
        bus.write16(0x0700_0000, 0);
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_FF00));
    }

    #[test]
    fn mode0_obj_transparency_preserves_background() {
        let mut bus = bus();
        bus.write16(0x0400_0000, (1 << 8) | (1 << 12));
        bus.write16(PALETTE_BASE, 0x7C00); // backdrop blue
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ index 1 red

        bus.write16(0x0700_0000, 0);
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0);
        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_00FF));
    }

    #[test]
    fn bitmap_mode4_renders_obj_with_obj_palette_and_transparency() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 4 | (1 << 12)); // mode 4, OBJ enabled
        bus.write16(VRAM_BASE, 0); // framebuffer index 0: backdrop color
        bus.write16(PALETTE_BASE, 0x7C00); // background palette index 0: blue
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ palette index 1: red
        bus.write16(0x0601_4000, 0x0001); // OBJ tile pixel index 1
        bus.write16(0x0601_4002, 0x0000); // next pixels transparent
        bus.write16(0x0700_0000, 0);
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 512); // first valid OBJ tile in bitmap modes

        let frame = render_bitmap_frame(&bus).expect("mode 4 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_00FF));
    }

    #[test]
    fn bitmap_mode5_obj_uses_bitmap_mode_character_base() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 5 | (1 << 12)); // mode 5, OBJ enabled
        bus.write16(VRAM_BASE, 0x03E0); // underlying framebuffer pixel: green
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ palette index 1: red
        bus.write16(0x0601_4000, 0x0001);
        bus.write16(0x0700_0000, 0);
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 512); // first valid OBJ tile in bitmap modes

        let frame = render_bitmap_frame(&bus).expect("mode 5 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_0000));
    }

    #[test]
    fn bitmap_mode3_renders_obj_from_bitmap_obj_vram_region() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 12)); // mode 3 and OBJ enabled
        bus.write16(VRAM_BASE, 0x03E0); // framebuffer pixel (0,0): green

        bus.write16(0x0601_4000, 0x0001);
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ palette index 1: red
        bus.write16(0x0700_0000, 0);
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 512);

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(rgb555(bus.read16(VRAM_BASE + 2))));
    }

    #[test]
    fn bitmap_mode_uses_bg2cnt_priority_for_obj_compositing() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10) | (1 << 12)); // Mode 3, BG2 and OBJ enabled
        bus.write16(BG0CNT + 4, 2); // BG2 priority 2
        bus.write16(VRAM_BASE, 0x03E0); // bitmap pixel 0 green
        bus.write16(VRAM_BASE + 8 * 2, 0x03E0); // bitmap pixel 8 green too
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ index 1: red
        bus.write16(PALETTE_BASE + 0x204, 0x7C00); // OBJ index 2: blue
        bus.write16(0x0601_4000, 0x1111); // tile 0: red
        bus.write16(0x0601_4020, 0x2222); // tile 1: blue

        // Priority 1 is in front of BG2 priority 2.
        bus.write16(0x0700_0000, 0);
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 512 | (1 << 10));
        // Priority 3 is behind BG2 and must not cover the bitmap.
        bus.write16(0x0700_0008, 0);
        bus.write16(0x0700_000A, 8);
        bus.write16(0x0700_000C, 513 | (3 << 10));

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(8, 0), Some(0xFF00_FF00));
    }

    #[test]
    fn bitmap_modes_keep_lower_priority_objects_behind_opaque_bg2() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10) | (1 << 12)); // mode 3, BG2 and OBJ enabled
        bus.write16(VRAM_BASE, 0x03E0); // BG2 framebuffer pixel: green
        bus.write16(0x0601_4000, 0x0001); // OBJ tile pixel 0: palette index 1
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ palette index 1: red
        bus.write16(0x0700_0000, 0); // visible, regular 8x8 OBJ
        bus.write16(0x0700_0002, 0); // regular 8x8 sprite, no flips
        bus.write16(0x0700_0004, 512 | (1 << 10)); // tile 512, priority 1, behind BG2

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_FF00));

        // Priority zero remains in front of the bitmap framebuffer.
        bus.write16(0x0700_0004, 512);
        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn bitmap_modes_reject_obj_tiles_overlapping_framebuffer_memory() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10) | (1 << 12)); // mode 3, BG2 and OBJ enabled
        bus.write16(VRAM_BASE, 0x03E0); // BG2 pixel is green
        bus.write16(0x0601_0000, 0x0001); // tile 0 overlaps the bitmap-reserved OBJ area
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ palette index 1: red
        bus.write16(0x0700_0000, 0); // visible regular sprite
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0); // tile index 0 is unavailable in bitmap modes

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_FF00));
    }

    #[test]
    fn bitmap_mode4_uses_selected_frame_page_and_palette() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 4 | (1 << 4) | (1 << 10)); // mode 4, BG2 enabled, page 1
        bus.write16(VRAM_BASE + 0xA000, 3);
        bus.write16(PALETTE_BASE + 6, 0x001F); // palette index 3: red

        let frame = render_bitmap_frame(&bus).expect("mode 4 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_0000));
    }

    #[test]
    fn bitmap_mode5_is_centered_and_uses_selected_page() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 5 | (1 << 4) | (1 << 10)); // mode 5, BG2 enabled, page 1
        bus.write16(VRAM_BASE + 0xA000, 0x001F); // first visible pixel: red

        let frame = render_bitmap_frame(&bus).expect("mode 5 should render");
        assert_eq!(frame.pixel(39, 16), Some(0xFF00_0000));
        assert_eq!(frame.pixel(40, 16), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(41, 16), Some(0xFF00_0000));
        assert_eq!(frame.pixel(200, 16), Some(0xFF00_0000));
    }

    #[test]
    fn lower_oam_index_wins_obj_priority_ties() {
        let mut bus = bus();
        // Entry 0 has the lower OAM index and must win the equal-priority tie.
        bus.write16(0x0400_0000, 1 << 12); // OBJ enabled
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // index 1 red
        bus.write16(PALETTE_BASE + 0x204, 0x03E0);
        bus.write16(0x0601_0000, 0x0001);
        bus.write16(0x0601_0020, 0x0002);
        bus.write16(0x0700_0000, 0);
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0);
        bus.write16(0x0700_0008, 0);
        bus.write16(0x0700_000A, 0);
        bus.write16(0x0700_000C, 1);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn only_first_32_oam_entries_are_evaluated_per_scanline() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12); // OBJ enabled
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // index 1: red
        bus.write16(PALETTE_BASE + 0x204, 0x7C00); // index 2: blue
        bus.write16(0x0601_0000, 0x1111); // tile 0 is red
        bus.write16(0x0601_0020, 0x2222); // tile 1 is blue

        // Thirty-two higher-priority OAM entries occupy the scanline.
        // Entry 32 is placed elsewhere on the same line and must be dropped.
        for index in 0..32_u32 {
            let base = 0x0700_0000 + index * 8;
            bus.write16(base, 0);
            bus.write16(base + 2, 0);
            bus.write16(base + 4, 0);
        }
        let dropped = 0x0700_0000 + 32 * 8;
        bus.write16(dropped, 0);
        bus.write16(dropped + 2, 8);
        bus.write16(dropped + 4, 1);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(8, 0), Some(0xFF00_0000));
    }

    #[test]
    fn non_normal_obj_modes_are_not_drawn_as_opaque_sprites() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12); // OBJ enabled
        bus.write16(PALETTE_BASE, 0x7C00); // blue backdrop
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ index 1 red
        bus.write16(0x0601_0000, 0x0001);
        bus.write16(0x0700_0000, 2 << 10); // OBJ-window mode is not drawn as a regular sprite
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_00FF));
    }

    #[test]
    fn mode0_renders_8bpp_obj_pixels_and_zero_transparency() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12); // OBJ enabled
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ palette index 1: red
        bus.write16(PALETTE_BASE + 0x204, 0x03E0); // OBJ palette index 2: green
        bus.write16(0x0601_0000, 0x0201); // 8bpp pixels 1, 2
        bus.write16(0x0601_0002, 0x0000); // transparent pixels
        bus.write16(0x0700_0000, 1 << 13); // 8bpp, regular 8x8
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_FF00));
        assert_eq!(frame.pixel(2, 0), Some(0xFF00_0000));
    }

    #[test]
    fn obj_rectangular_size_renders_expected_extent() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12); // OBJ enabled
        bus.write16(PALETTE_BASE + 0x202, 0x001F);
        // Fill the first 16x8 sprite row with palette index 1.
        for offset in (0..64).step_by(2) {
            bus.write16(0x0601_0000 + offset, 0x1111);
        }
        bus.write16(0x0700_0000, 1 << 14); // horizontal shape, size 0 = 16x8
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(15, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(16, 0), Some(0xFF00_0000));
        assert_eq!(frame.pixel(15, 7), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(0, 8), Some(0xFF00_0000));
    }

    #[test]
    fn mode0_obj_vflip_reverses_4bpp_rows() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12); // OBJ enabled
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // index 1 red
        bus.write16(PALETTE_BASE + 0x204, 0x03E0); // index 2 green

        bus.write16(0x0601_0000, 0x0001);
        bus.write16(0x0601_001C, 0x0002);
        bus.write16(0x0700_0000, 0);
        bus.write16(0x0700_0002, 1 << 13); // vertical flip
        bus.write16(0x0700_0004, 0);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_FF00));
        assert_eq!(frame.pixel(0, 7), Some(0xFFFF_0000));
    }

    #[test]
    fn regular_obj_8bpp_uses_even_tile_index_and_palette_indices() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12); // OBJ enabled
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ index 1: red
        bus.write16(PALETTE_BASE + 0x204, 0x03E0); // OBJ index 2: green
                                                   // An 8bpp tile occupies 64 bytes and its tile number is rounded down
                                                   // to an even 32-byte slot.
        bus.write16(0x0601_0000, 0x0201);
        bus.write16(0x0700_0000, 1 << 13); // 8bpp OBJ
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 1); // odd tile index aliases tile 0

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFF00_FF00));
        assert_eq!(frame.pixel(2, 0), Some(0xFF00_0000));
    }

    #[test]
    fn affine_obj_identity_matrix_renders_regular_sprite_coordinates() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12); // OBJ enabled
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ index 1: red
        bus.write16(0x0601_0000, 0x1111); // source pixels use palette index 1
        bus.write16(0x0700_0000, 1 << 8); // affine 8x8 OBJ
        bus.write16(0x0700_0002, 0); // matrix 0, position (0,0)
        bus.write16(0x0700_0004, 0); // tile 0, priority 0
        bus.write16(0x0700_0006, 0x0100); // PA = 1.0
        bus.write16(0x0700_000E, 0); // PB = 0
        bus.write16(0x0700_0016, 0); // PC = 0
        bus.write16(0x0700_001E, 0x0100); // PD = 1.0

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn affine_obj_double_size_expands_destination_bounds() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12); // OBJ enabled
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ index 1: red
        for y in 0..8_u32 {
            for x in 0..4_u32 {
                bus.write8(0x0601_0000 + y * 4 + x, 0x11);
            }
        }
        bus.write16(0x0700_0000, (1 << 8) | (1 << 9)); // affine + double-size
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0);
        bus.write16(0x0700_0006, 0x0100);
        bus.write16(0x0700_000E, 0);
        bus.write16(0x0700_0016, 0);
        bus.write16(0x0700_001E, 0x0100);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        // The doubled 16x16 box centers the original 8x8 image at (4,4).
        assert_eq!(frame.pixel(4, 4), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_0000));
    }

    #[test]
    fn regular_obj_mosaic_repeats_source_pixels_and_rows() {
        let mut bus = bus();
        bus.write16(0x0400_0000, (1 << 6) | (1 << 12)); // OBJ enabled, 1D mapping
        bus.write16(0x0400_004C, 1 << 8 | 1 << 12); // OBJ mosaic 2x2
        bus.write16(0x0700_0000, 1 << 12); // OBJ mosaic enabled
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0);
        bus.write16(0x0601_0000, 0x0021); // source pixels 1, 2; next byte is zero
        bus.write16(0x0601_0004, 0x0043); // next source row: pixels 3, 4
        bus.write16(PALETTE_BASE + 0x202, 0x001F);
        bus.write16(PALETTE_BASE + 0x204, 0x03E0);
        bus.write16(PALETTE_BASE + 0x206, 0x7C00);
        bus.write16(PALETTE_BASE + 0x208, 0x7FFF);

        let frame = render_bitmap_frame(&bus).unwrap();
        assert_eq!(frame.pixel(0, 0), frame.pixel(1, 0));
        assert_eq!(frame.pixel(0, 0), frame.pixel(0, 1));
        assert_ne!(frame.pixel(0, 0), frame.pixel(2, 0));
    }

    #[test]
    fn mode0_obj_hflip_reverses_4bpp_pixels() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12); // OBJ enabled
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // index 1 red
        bus.write16(PALETTE_BASE + 0x204, 0x03E0); // index 2 green

        bus.write16(0x0601_0000, 0x0021);
        bus.write16(0x0601_0002, 0x0000);
        bus.write16(0x0601_0004, 0x0000);
        bus.write16(0x0601_0006, 0x0000);
        bus.write16(0x0601_0008, 0x0000);
        bus.write16(0x0601_000A, 0x0000);
        bus.write16(0x0601_000C, 0x0000);
        bus.write16(0x0601_000E, 0x0000);
        bus.write16(0x0700_0000, 0);
        bus.write16(0x0700_0002, 1 << 12); // horizontal flip
        bus.write16(0x0700_0004, 0);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_0000));
        assert_eq!(frame.pixel(6, 0), Some(0xFF00_FF00));
        assert_eq!(frame.pixel(7, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn unsupported_display_mode_is_blank_only_when_forced_blank_is_set() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 6);
        assert!(render_bitmap_frame(&bus).is_none());

        bus.write16(0x0400_0000, 6 | (1 << 7));
        let frame = render_bitmap_frame(&bus).expect("forced blank is valid in any mode");
        assert!(frame.pixels.iter().all(|pixel| *pixel == 0xFFFF_FFFF));
    }
    #[test]
    fn semi_transparent_obj_blends_with_backdrop_when_bg2_is_disabled() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 12)); // Mode 3, OBJ enabled, BG2 disabled.
        bus.write16(PALETTE_BASE, 0x03E0); // Green backdrop.
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // Red OBJ palette index 1.
        bus.write16(0x0601_4000, 0x1111); // Bitmap-mode OBJ tile index 512.
        bus.write16(0x0700_0000, 1 << 10); // Semi-transparent 8x8 OBJ.
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 512);
        bus.write16(0x0400_0050, (1 << 6) | (1 << 13)); // Backdrop is target 2.
        bus.write16(0x0400_0052, 8 | (8 << 8)); // EVA = EVB = 8.

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF7B_7B00));
    }

    #[test]
    fn semi_transparent_obj_blends_with_bitmap_bg2_when_selected_as_second_target() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10) | (1 << 12)); // Mode 3, BG2 and OBJ
        bus.write16(VRAM_BASE, 0x03E0); // Green BG2 pixel
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // Red OBJ palette index 1
        bus.write16(0x0601_4000, 0x1111); // Bitmap-mode OBJ tile index 512
        bus.write16(0x0700_0000, 1 << 10); // Semi-transparent 8x8 OBJ
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 512);
        bus.write16(0x0400_0050, (1 << 6) | (1 << 10)); // Alpha blend; BG2 is target 2
        bus.write16(0x0400_0052, 8 | (8 << 8)); // EVA = EVB = 8

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF7B_7B00));
    }

    #[test]
    fn semi_transparent_obj_blends_in_mode4_with_selected_bitmap_page() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 4 | (1 << 4) | (1 << 10) | (1 << 12));
        bus.write8(VRAM_BASE + 0xA000, 2); // Page 1 BG pixel uses palette index 2.
        bus.write16(PALETTE_BASE + 4, 0x03E0); // BG green.
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // OBJ red.
        bus.write16(0x0601_4000, 0x1111); // Bitmap-mode OBJ tile 512.
        bus.write16(0x0700_0000, 1 << 10); // Semi-transparent OBJ.
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 512);
        bus.write16(0x0400_0050, (1 << 6) | (1 << 10));
        bus.write16(0x0400_0052, 8 | (8 << 8));

        let frame = render_bitmap_frame(&bus).expect("mode 4 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF7B_7B00));
    }

    #[test]
    fn semi_transparent_obj_does_not_blend_when_bg2_is_not_selected_as_second_target() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10) | (1 << 12));
        bus.write16(VRAM_BASE, 0x03E0); // Green bitmap pixel.
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // Red OBJ.
        bus.write16(0x0601_4000, 0x1111);
        bus.write16(0x0700_0000, 1 << 10);
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 512);
        bus.write16(0x0400_0050, 1 << 6); // Alpha blend, but BG2 is not target 2.
        bus.write16(0x0400_0052, 8 | (8 << 8));

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn semi_transparent_obj_does_not_treat_bg0_as_bg2_target() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10) | (1 << 12));
        bus.write16(VRAM_BASE, 0x03E0);
        bus.write16(PALETTE_BASE + 0x202, 0x001F);
        bus.write16(0x0601_4000, 0x1111);
        bus.write16(0x0700_0000, 1 << 10);
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 512);
        // BLDCNT target 2 bit 8 selects BG0, not BG2.
        bus.write16(0x0400_0050, (1 << 6) | (1 << 8));
        bus.write16(0x0400_0052, 8 | (8 << 8));

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn text_mode_semi_transparent_obj_ignores_brightness_effects() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12); // Mode 0 with OBJ enabled, BGs disabled.
        bus.write16(PALETTE_BASE + 0x202, 0x0001); // Very dark red OBJ.
        bus.write16(0x0601_0000, 0x1111); // OBJ tile 0, color index 1.
        bus.write16(0x0700_0000, 1 << 10); // Semi-transparent OBJ.
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0);
        bus.write16(0x0400_0050, (2 << 6) | (1 << 4)); // Brighten, OBJ first target.
        bus.write16(0x0400_0054, 16); // Maximum brightness.

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(rgb555(0x0001)));
    }

    #[test]
    fn semi_transparent_obj_ignores_brightness_when_alpha_is_not_enabled() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10) | (1 << 12)); // Mode 3, BG2 and OBJ.
        bus.write16(VRAM_BASE, 0x03E0); // Green BG2 pixel.
        bus.write16(PALETTE_BASE + 0x202, 0x0001); // Very dark red OBJ.
        bus.write16(0x0601_4000, 0x1111);
        bus.write16(0x0700_0000, 1 << 10); // Semi-transparent OBJ.
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 512);
        bus.write16(0x0400_0050, (2 << 6) | (1 << 4)); // Brighten, OBJ first target.
        bus.write16(0x0400_0054, 16); // Maximum brightness.

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(rgb555(0x0001)));
    }

    #[test]
    fn semi_transparent_obj_requires_alpha_effect_mode() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10) | (1 << 12));
        bus.write16(VRAM_BASE, 0x03E0);
        bus.write16(PALETTE_BASE + 0x202, 0x001F);
        bus.write16(0x0601_4000, 0x1111);
        bus.write16(0x0700_0000, 1 << 10);
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 512);
        bus.write16(0x0400_0050, (2 << 6) | (1 << 10)); // Brighten, not alpha.
        bus.write16(0x0400_0052, 8 | (8 << 8));

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn disabled_and_prohibited_objects_do_not_consume_scanline_limit() {
        let mut oam = [0_u8; 1024];
        for index in 0..16_usize {
            let base = index * 8;
            // Prohibited OBJ mode 3.
            oam[base..base + 2].copy_from_slice(&(3_u16 << 10).to_le_bytes());
            // Non-affine disabled OBJ entries.
            let disabled = (16 + index) * 8;
            oam[disabled..disabled + 2].copy_from_slice(&(1_u16 << 9).to_le_bytes());
        }
        let allowed = object_scanline_mask(&oam);
        assert!(allowed[32][0]);
    }

    #[test]
    fn obj_window_entries_consume_scanline_object_limit() {
        let mut oam = [0_u8; 1024];
        for index in 0..32_usize {
            let base = index * 8;
            // OBJ mode 2 is an OBJ-window entry; it still occupies an OAM slot.
            oam[base..base + 2].copy_from_slice(&(2_u16 << 10).to_le_bytes());
            oam[base + 2..base + 4].copy_from_slice(&0_u16.to_le_bytes());
        }
        let visible_object = 32 * 8;
        oam[visible_object..visible_object + 2].copy_from_slice(&8_u16.to_le_bytes());
        oam[visible_object + 2..visible_object + 4].copy_from_slice(&0_u16.to_le_bytes());

        let allowed = object_scanline_mask(&oam);
        assert!(allowed[..32].iter().all(|lines| lines[0]));
        assert!(!allowed[32][0]);
        assert!(allowed[32][8]);
    }

    #[test]
    fn object_scanline_limit_resets_for_each_scanline() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12);
        bus.write16(PALETTE_BASE + 0x202, 0x001F);
        bus.write16(0x0601_0000, 0x1111);
        // First 32 objects occupy only scanline zero (8x8 sprites).
        for index in 0..32_u32 {
            let base = 0x0700_0000 + index * 8;
            bus.write16(base, 0);
            bus.write16(base + 2, 0);
            bus.write16(base + 4, 0);
        }
        // Object 32 is vertically displaced and should be evaluated on line 8.
        let next_line = 0x0700_0000 + 32 * 8;
        bus.write16(next_line, 8);
        bus.write16(next_line + 2, 0);
        bus.write16(next_line + 4, 0);

        let frame = render_bitmap_frame(&bus).expect("mode 0 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(0, 8), Some(0xFFFF_0000));
    }

    #[test]
    fn bitmap_brightness_effect_respects_first_target_and_effect_mode() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 10);
        bus.write16(0x0400_0050, (1 << 2) | (2 << 6));
        bus.write16(0x0400_0054, 16);
        assert_eq!(
            bitmap_brightness_effect(&bus, bus.read16(0x0400_0000), 2),
            Some((true, 16))
        );
        bus.write16(0x0400_0050, (1 << 6) | (1 << 2));
        assert_eq!(
            bitmap_brightness_effect(&bus, bus.read16(0x0400_0000), 2),
            None
        );
        bus.write16(0x0400_0050, 2 << 6);
        assert_eq!(
            bitmap_brightness_effect(&bus, bus.read16(0x0400_0000), 2),
            None
        );
    }

    #[test]
    fn bitmap_obj_brightness_requires_obj_first_target_selection() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 12);
        bus.write16(0x0400_0050, (1 << 4) | (2 << 6));
        bus.write16(0x0400_0054, 8);
        assert_eq!(
            bitmap_brightness_effect(&bus, bus.read16(0x0400_0000), 4),
            Some((true, 8))
        );
        bus.write16(0x0400_0050, (1 << 2) | (2 << 6));
        assert_eq!(
            bitmap_brightness_effect(&bus, bus.read16(0x0400_0000), 4),
            None
        );
    }

    #[test]
    fn backdrop_brightness_requires_backdrop_first_target_selection() {
        let mut bus = bus();
        bus.write16(0x0400_0050, (1 << 5) | (2 << 6));
        bus.write16(0x0400_0054, 16);
        assert_eq!(
            bitmap_brightness_effect(&bus, bus.read16(0x0400_0000), 5),
            Some((true, 16))
        );
        bus.write16(0x0400_0050, (1 << 2) | (2 << 6));
        assert_eq!(
            bitmap_brightness_effect(&bus, bus.read16(0x0400_0000), 5),
            None
        );
    }

    #[test]
    fn bitmap_brightness_and_darkness_reach_expected_rgb555_limits() {
        let black = 0xFF00_0000;
        let white = 0xFFFFFFFF;
        assert_eq!(apply_brightness_effect(black, Some((true, 16))), white);
        assert_eq!(apply_brightness_effect(white, Some((false, 16))), black);
        assert_eq!(apply_brightness_effect(0xFF808080, None), 0xFF808080);
    }

    #[test]
    fn bitmap_alpha_blend_clamps_coefficients_and_blends_5bit_channels() {
        assert_eq!(blend_argb555(0xFFFF_0000, 0xFF00_0000, 8, 8), 0xFF7B_0000);
        assert_eq!(blend_argb555(0xFFFF_FFFF, 0xFFFF_FFFF, 31, 31), 0xFFFF_FFFF);
        assert_eq!(blend_argb555(0xFFFF_0000, 0xFF00_0000, 0, 16), 0xFF00_0000);
    }

    #[test]
    fn text_background_brightness_only_affects_selected_first_targets() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 1 << 8); // Mode 0, BG0 enabled.
        bus.write16(VRAM_BASE, 1); // BG0 map selects tile 1.
        bus.write8(VRAM_BASE + 32, 1); // Tile 1 pixel 0 uses palette entry 1.
        bus.write16(PALETTE_BASE + 2, 0x03E0); // Green.
        bus.write16(0x0400_0050, (1 << 0) | (2 << 6)); // BG0 target, brighten.
        bus.write16(0x0400_0054, 16);

        let bright = render_bitmap_frame(&bus).unwrap();
        assert_eq!(bright.pixel(0, 0), Some(0xFFFFFFFF));

        bus.write16(0x0400_0050, (1 << 1) | (2 << 6)); // BG1 selected, not BG0.
        let unchanged = render_bitmap_frame(&bus).unwrap();
        assert_eq!(unchanged.pixel(0, 0), Some(0xFF00_FF00));
    }

    #[test]
    fn affine_background_brightness_uses_bg2_target_bit() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 2 | (1 << 10)); // Mode 2, BG2 enabled.
        bus.write16(BG0CNT + 4, 0); // BG2 priority 0, affine map at block 0.
        bus.write8(VRAM_BASE, 1); // First map entry selects tile 1.
        bus.write8(VRAM_BASE + 64, 1); // Tile 1 pixel 0 uses palette entry 1.
        bus.write16(PALETTE_BASE + 2, 0x03E0); // Green.
        bus.write16(0x0400_0050, (1 << 2) | (2 << 6)); // BG2 target, brighten.
        bus.write16(0x0400_0054, 16);

        let frame = render_bitmap_frame(&bus).unwrap();
        assert_eq!(frame.pixel(0, 0), Some(0xFFFFFFFF));
    }
    #[test]
    fn obj_window_reveals_bg2_only_under_opaque_obj_pixels() {
        let mut bus = bus();
        // Mode 3 with BG2, OBJ, and OBJ window enabled.
        bus.write16(0x0400_0000, 3 | (1 << 10) | (1 << 12) | (1 << 15));
        bus.write16(VRAM_BASE, 0x001F);
        bus.write16(VRAM_BASE + 2, 0x001F);
        bus.write16(VRAM_BASE + 8 * 2, 0x001F);
        bus.write16(PALETTE_BASE, 0x03E0);
        // WINOBJ allows BG2; outside the OBJ window, BG2 is hidden.
        bus.write16(0x0400_004A, (1 << 2) << 8);
        // An 8x8 OBJ-window sprite using tile 512, the first available OBJ
        // tile in bitmap mode. Color index 1 is opaque; index 0 stays transparent.
        bus.write16(0x0700_0000, 2 << 10);
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 512);
        bus.write16(VRAM_BASE + 0x14000, 0x1111);

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(1, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(8, 0), Some(0xFF00_FF00));
    }

    #[test]
    fn win0_color_effect_bit_disables_brightness_only_inside_window() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10) | (1 << 13)); // Mode 3, BG2, WIN0.
        bus.write16(VRAM_BASE, 0x001F); // Red bitmap pixel.
        bus.write16(VRAM_BASE + 8 * 2, 0x001F);
        bus.write16(0x0400_0040, 8 << 8); // WIN0 horizontal range [0,8).
        bus.write16(0x0400_0044, 1 << 8); // WIN0 vertical range [0,1).
        bus.write16(0x0400_0048, 1 << 2); // WIN0 allows BG2 but disables effects.
        bus.write16(0x0400_004A, (1 << 2) | (1 << 5)); // Outside allows BG2 and effects.
        bus.write16(0x0400_0050, (1 << 2) | (2 << 6)); // BG2 target, brighten.
        bus.write16(0x0400_0054, 16);

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(8, 0), Some(0xFFFFFFFF));
    }

    #[test]
    fn win0_masks_selected_background_pixels_without_erasing_backdrop() {
        let mut bus = bus();
        bus.write16(0x0400_0000, (1 << 8) | (1 << 13)); // BG0 and WIN0.
        bus.write16(VRAM_BASE, 1);
        bus.write8(VRAM_BASE + 32, 1);
        bus.write16(PALETTE_BASE + 2, 0x001F); // Red BG0.
        bus.write16(0x0400_0040, 8 << 8); // WIN0 horizontal range [0,8).
        bus.write16(0x0400_0044, 1 << 8); // WIN0 vertical range [0,1).
        bus.write16(0x0400_0048, 0); // WIN0 hides all layers.
        bus.write16(0x0400_004A, (1 << 0) | (1 << 5)); // Outside: BG0 and color effects.
        bus.write16(PALETTE_BASE, 0x03E0); // Green backdrop.

        let frame = render_bitmap_frame(&bus).unwrap();
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_FF00));
        assert_eq!(frame.pixel(8, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn win0_horizontal_range_wraps_across_screen_edges() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10) | (1 << 13)); // BG2 and WIN0.
        bus.write16(PALETTE_BASE, 0x03E0); // Green backdrop.
        bus.write16(VRAM_BASE, 0x001F); // Red bitmap.
        bus.write16(VRAM_BASE + 5 * 2, 0x001F);
        bus.write16(0x0400_0040, (4 << 8) | 236); // X1=236, X2=4.
        bus.write16(0x0400_0044, 1 << 8); // Y1=0, Y2=1.
        bus.write16(0x0400_0048, 0); // WIN0 hides BG2.
        bus.write16(0x0400_004A, 1 << 2); // Outside shows BG2.

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_FF00));
        assert_eq!(frame.pixel(5, 0), Some(0xFFFF_0000));
        assert_eq!(frame.pixel(239, 0), Some(0xFF00_FF00));
    }

    #[test]
    fn win1_masks_bitmap_background_inside_its_horizontal_range() {
        let mut bus = bus();
        bus.write16(0x0400_0000, 3 | (1 << 10) | (1 << 14)); // BG2 and WIN1.
        bus.write16(PALETTE_BASE, 0x03E0); // Green backdrop.
        bus.write16(VRAM_BASE, 0x001F); // Red bitmap pixel.
        bus.write16(VRAM_BASE + 4 * 2, 0x001F);
        bus.write16(0x0400_0042, 4 << 8); // WIN1 horizontal range [0,4).
        bus.write16(0x0400_0046, 1 << 8); // WIN1 vertical range [0,1).
        bus.write16(0x0400_0048, 1 << 8); // WIN1 allows BG0 but not BG2.
        bus.write16(0x0400_004A, 1 << 2); // Outside allows BG2.

        let frame = render_bitmap_frame(&bus).expect("mode 3 should render");
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_FF00));
        assert_eq!(frame.pixel(4, 0), Some(0xFFFF_0000));
    }

    #[test]
    fn win0_can_hide_obj_pixels_while_leaving_background_visible() {
        let mut bus = bus();
        bus.write16(0x0400_0000, (1 << 6) | (1 << 12) | (1 << 13)); // 1D OBJ, OBJ and WIN0.
        bus.write16(PALETTE_BASE, 0x03E0); // Green backdrop.
        bus.write16(PALETTE_BASE + 0x202, 0x001F); // Red OBJ.
        for offset in (0..4).map(|index| index * 2) {
            bus.write16(0x0601_0000 + offset, 0x1111);
        }
        bus.write16(0x0700_0000, 0);
        bus.write16(0x0700_0002, 0);
        bus.write16(0x0700_0004, 0);
        bus.write16(0x0400_0040, 4 << 8);
        bus.write16(0x0400_0044, 1 << 8);
        bus.write16(0x0400_0048, 1 << 5); // Backdrop visible; OBJ hidden.
        bus.write16(0x0400_004A, (1 << 4) | (1 << 5)); // Outside: OBJ and backdrop.

        let frame = render_bitmap_frame(&bus).unwrap();
        assert_eq!(frame.pixel(0, 0), Some(0xFF00_FF00));
        assert_eq!(frame.pixel(4, 0), Some(0xFFFF_0000));
    }
}
