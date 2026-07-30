//! The raster bits of the site: index thumbnails, the Open Graph cards, and
//! the favicon. Screenshots plus filled rectangles for the RCT window chrome,
//! and ab_glyph for the text overlaid on the head-to-head card (embedded IBM
//! Plex Mono, the same face the site uses).

use std::path::Path;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::{DynamicImage, Rgb, RgbImage};

const PANEL: Rgb<u8> = Rgb([0xc2, 0xb2, 0x80]);
const PANEL_HI: Rgb<u8> = Rgb([0xdc, 0xcf, 0xa4]);
const PANEL_DARK: Rgb<u8> = Rgb([0x8c, 0x7a, 0x52]);
const TITLEBAR: Rgb<u8> = Rgb([0x5c, 0x3a, 0x24]);
const TITLEBAR_HI: Rgb<u8> = Rgb([0x8a, 0x5f, 0x3d]);
const INK: Rgb<u8> = Rgb([0x2a, 0x20, 0x15]);
const SKY: Rgb<u8> = Rgb([0x10, 0x20, 0x2e]);
/// Warm cream for text on the brown titlebar; gold for the winner's numbers.
const CREAM: Rgb<u8> = Rgb([0xf0, 0xe6, 0xc8]);
const GOLD: Rgb<u8> = Rgb([0xf2, 0xc9, 0x4c]);

const FONT_REGULAR: &[u8] = include_bytes!("../fonts/IBMPlexMono-Regular.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../fonts/IBMPlexMono-Bold.ttf");

/// The two faces, parsed once per card render.
struct Fonts {
    regular: FontRef<'static>,
    bold: FontRef<'static>,
}

impl Fonts {
    fn load() -> Result<Self> {
        Ok(Self {
            regular: FontRef::try_from_slice(FONT_REGULAR).context("parsing Plex Mono Regular")?,
            bold: FontRef::try_from_slice(FONT_BOLD).context("parsing Plex Mono Bold")?,
        })
    }
}

/// Horizontal placement of a text run relative to the given x.
#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
    Right,
}

/// Total advance width of `text` at `size`, for alignment and plate sizing.
fn text_width(font: &FontRef<'static>, size: f32, text: &str) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    text.chars()
        .map(|c| scaled.h_advance(scaled.glyph_id(c)))
        .sum()
}

/// Alpha-blends `colour` over one pixel at coverage `a` (0..=1), clipped.
fn blend_px(img: &mut RgbImage, x: i64, y: i64, colour: Rgb<u8>, a: f32) {
    if a <= 0.0 || x < 0 || y < 0 || x >= img.width() as i64 || y >= img.height() as i64 {
        return;
    }
    let a = a.min(1.0);
    let bg = img.get_pixel(x as u32, y as u32).0;
    let mix = |s: u8, d: u8| (s as f32 * (1.0 - a) + d as f32 * a).round() as u8;
    img.put_pixel(
        x as u32,
        y as u32,
        Rgb([
            mix(bg[0], colour.0[0]),
            mix(bg[1], colour.0[1]),
            mix(bg[2], colour.0[2]),
        ]),
    );
}

/// A translucent rectangle: a dark scrim behind text so it reads over the
/// coaster, or any tinted plate.
fn blend_rect(img: &mut RgbImage, x0: i64, y0: i64, x1: i64, y1: i64, colour: Rgb<u8>, a: f32) {
    for y in y0..=y1 {
        for x in x0..=x1 {
            blend_px(img, x, y, colour, a);
        }
    }
}

/// Draws `text` with its baseline at `y`, anchored at `x` per `align`. A 1px
/// ink shadow keeps light text legible on light patches. Returns the run width.
fn draw_text(
    img: &mut RgbImage,
    font: &FontRef<'static>,
    size: f32,
    at: (i64, i64),
    colour: Rgb<u8>,
    align: Align,
    text: &str,
) -> f32 {
    let (x, y) = at;
    let width = text_width(font, size, text);
    let start = match align {
        Align::Left => x as f32,
        Align::Center => x as f32 - width / 2.0,
        Align::Right => x as f32 - width,
    };
    let scaled = font.as_scaled(PxScale::from(size));
    let mut pen = start;
    for ch in text.chars() {
        let glyph = scaled.scaled_glyph(ch);
        let advance = scaled.h_advance(glyph.id);
        if let Some(outline) = font.outline_glyph(ab_glyph::Glyph {
            position: ab_glyph::point(pen, y as f32),
            ..glyph
        }) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, cov| {
                let px = bounds.min.x as i64 + gx as i64;
                let py = bounds.min.y as i64 + gy as i64;
                // Drop shadow first, then the glyph, so edges stay crisp.
                blend_px(img, px + 1, py + 1, INK, cov * 0.55);
                blend_px(img, px, py, colour, cov);
            });
        }
        pen += advance;
    }
    width
}

/// Fills an inclusive rectangle, clipped to the image.
fn fill(img: &mut RgbImage, x0: i64, y0: i64, x1: i64, y1: i64, colour: Rgb<u8>) {
    let (w, h) = (img.width() as i64, img.height() as i64);
    for y in y0.max(0)..=y1.min(h - 1) {
        for x in x0.max(0)..=x1.min(w - 1) {
            img.put_pixel(x as u32, y as u32, colour);
        }
    }
}

/// Draws an inclusive rectangle outline `width` pixels thick, inwards.
fn outline(img: &mut RgbImage, x0: i64, y0: i64, x1: i64, y1: i64, width: i64, colour: Rgb<u8>) {
    fill(img, x0, y0, x1, y0 + width - 1, colour);
    fill(img, x0, y1 - width + 1, x1, y1, colour);
    fill(img, x0, y0, x0 + width - 1, y1, colour);
    fill(img, x1 - width + 1, y0, x1, y1, colour);
}

/// Scales to cover `w`x`h` and center-crops. NEAREST when upscaling keeps the
/// pixel art crisp; LANCZOS is better going down.
fn cover_crop(img: &DynamicImage, w: u32, h: u32) -> DynamicImage {
    let scale = (w as f64 / img.width() as f64).max(h as f64 / img.height() as f64);
    let filter = if scale > 1.0 {
        FilterType::Nearest
    } else {
        FilterType::Lanczos3
    };
    let scaled = img.resize_exact(
        ((img.width() as f64 * scale).round() as u32).max(w),
        ((img.height() as f64 * scale).round() as u32).max(h),
        filter,
    );
    let x = (scaled.width() - w) / 2;
    let y = (scaled.height() - h) / 2;
    scaled.crop_imm(x, y, w, h)
}

/// A 32x32 RCT-style window (tan bevel panel + brown titlebar) as favicon.ico.
pub fn write_favicon(out: &Path) -> Result<()> {
    let mut img = RgbImage::from_pixel(32, 32, SKY);
    fill(&mut img, 2, 4, 29, 27, PANEL);
    outline(&mut img, 2, 4, 29, 27, 2, INK);
    fill(&mut img, 4, 6, 27, 11, TITLEBAR);
    fill(&mut img, 4, 6, 27, 6, TITLEBAR_HI);
    fill(&mut img, 4, 25, 27, 25, PANEL_DARK);
    fill(&mut img, 27, 8, 27, 25, PANEL_DARK);
    fill(&mut img, 4, 8, 4, 24, PANEL_HI);
    let path = out.join("favicon.ico");
    DynamicImage::ImageRgb8(img)
        .save(&path)
        .with_context(|| format!("writing {}", path.display()))
}

const OG_W: u32 = 1200;
const OG_H: u32 = 630;
const OG_OUTER: i64 = 3;
const OG_BEVEL: i64 = 6;

/// Draws the RCT-style beveled panel frame onto a fresh 1200x630 card and
/// returns it plus the inclusive interior rect the screenshot(s) fill.
fn og_frame() -> (RgbImage, (i64, i64, i64, i64)) {
    let mut card = RgbImage::from_pixel(OG_W, OG_H, PANEL);
    let (w, h) = (OG_W as i64 - 1, OG_H as i64 - 1);
    outline(&mut card, 0, 0, w, h, OG_OUTER, INK);
    outline(
        &mut card,
        OG_OUTER,
        OG_OUTER,
        w - OG_OUTER,
        h - OG_OUTER,
        OG_BEVEL,
        PANEL_HI,
    );
    // Bevel shading: dark on the bottom/right edges for the classic inset look.
    fill(
        &mut card,
        OG_OUTER,
        h - OG_OUTER - OG_BEVEL,
        w - OG_OUTER,
        h - OG_OUTER,
        PANEL_DARK,
    );
    fill(
        &mut card,
        w - OG_OUTER - OG_BEVEL,
        OG_OUTER,
        w - OG_OUTER,
        h - OG_OUTER,
        PANEL_DARK,
    );
    let inset = OG_OUTER + OG_BEVEL + 2;
    (card, (inset, inset, w - inset, h - inset))
}

/// Renders a single screenshot center-cropped to fill the framed card.
pub fn write_og_card(shot: &Path, dest: &Path) -> Result<()> {
    let (mut card, (x0, y0, x1, y1)) = og_frame();
    let (inner_w, inner_h) = ((x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
    let shot_img = image::open(shot).with_context(|| format!("reading {}", shot.display()))?;
    // Cover-fit so the whole ride reads at a glance (captures are cropped to
    // track bounds already).
    let cropped = cover_crop(&shot_img, inner_w, inner_h).to_rgb8();
    image::imageops::replace(&mut card, &cropped, x0, y0);
    save_card(card, dest)
}

/// One side of the head-to-head card: a coaster plus who built it and its
/// score. `winner` promotes the score badge to gold.
pub struct CardSide<'a> {
    pub shot: &'a Path,
    pub model: &'a str,
    pub score: Option<f64>,
    pub winner: bool,
}

/// Renders the head-to-head card: a brown titlebar ribbon across the top, then
/// the two coasters side by side, each with its score badged in the outer top
/// corner and the model name plated along the centre divider.
pub fn write_compare_card(
    coaster: &str,
    mode: &str,
    left: &CardSide,
    right: &CardSide,
    dest: &Path,
) -> Result<()> {
    let fonts = Fonts::load()?;
    let (mut card, (x0, y0, x1, y1)) = og_frame();

    // Titlebar ribbon.
    const RIBBON_H: i64 = 72;
    let ry = y0 + RIBBON_H - 1;
    fill(&mut card, x0, y0, x1, ry, TITLEBAR);
    fill(&mut card, x0, y0, x1, y0 + 2, TITLEBAR_HI);
    fill(&mut card, x0, ry - 2, x1, ry, INK);
    let cx = (x0 + x1) / 2;
    draw_text(
        &mut card,
        &fonts.bold,
        30.0,
        (cx, y0 + 34),
        GOLD,
        Align::Center,
        "HEAD TO HEAD",
    );
    let subtitle = format!("{coaster}  ·  {mode} mode");
    draw_text(
        &mut card,
        &fonts.regular,
        20.0,
        (cx, y0 + 60),
        CREAM,
        Align::Center,
        &subtitle,
    );

    // Two coaster panels below the ribbon.
    const GAP: i64 = 6;
    let py0 = ry + 1;
    let span = x1 - x0 + 1;
    let cell_w = ((span - GAP) / 2) as u32;
    let cell_h = (y1 - py0 + 1) as u32;
    let sides = [(x0, left), (x0 + cell_w as i64 + GAP, right)];
    for (i, (px, side)) in sides.iter().enumerate() {
        let img =
            image::open(side.shot).with_context(|| format!("reading {}", side.shot.display()))?;
        let cropped = cover_crop(&img, cell_w, cell_h).to_rgb8();
        image::imageops::replace(&mut card, &cropped, *px, py0);
        let is_left = i == 0;
        let panel_x1 = *px + cell_w as i64 - 1;
        draw_score_badge(&mut card, &fonts, *px, py0, panel_x1, side, is_left);
        draw_name_plate(&mut card, &fonts, *px, y1, panel_x1, side.model, is_left);
    }

    // Beveled divider between the panels.
    let dx = x0 + cell_w as i64;
    fill(&mut card, dx, py0, dx + GAP - 1, y1, PANEL_DARK);
    fill(&mut card, dx, py0, dx, y1, INK);
    fill(&mut card, dx + GAP - 1, py0, dx + GAP - 1, y1, PANEL_HI);

    save_card(card, dest)
}

/// The excitement badge in a panel's outer top corner: a beveled RCT plate
/// (gold for the winner) with the score, or "not rated" when there is none.
fn draw_score_badge(
    img: &mut RgbImage,
    fonts: &Fonts,
    panel_x0: i64,
    panel_y0: i64,
    panel_x1: i64,
    side: &CardSide,
    is_left: bool,
) {
    const W: i64 = 168;
    const H: i64 = 78;
    const M: i64 = 14; // margin from the panel corner
    let (bx0, bx1) = if is_left {
        (panel_x0 + M, panel_x0 + M + W)
    } else {
        (panel_x1 - M - W, panel_x1 - M)
    };
    let (by0, by1) = (panel_y0 + M, panel_y0 + M + H);
    let face = if side.winner { GOLD } else { PANEL };
    fill(img, bx0, by0, bx1, by1, face);
    outline(img, bx0, by0, bx1, by1, 3, INK);
    fill(img, bx0 + 3, by0 + 3, bx1 - 3, by0 + 4, PANEL_HI);
    fill(img, bx0 + 3, by1 - 4, bx1 - 3, by1 - 3, PANEL_DARK);
    let mid = (bx0 + bx1) / 2;
    match side.score {
        Some(score) => {
            draw_text(
                img,
                &fonts.regular,
                15.0,
                (mid, by0 + 24),
                INK,
                Align::Center,
                "EXCITEMENT",
            );
            draw_text(
                img,
                &fonts.bold,
                42.0,
                (mid, by1 - 16),
                INK,
                Align::Center,
                &format!("{score:.2}"),
            );
            if side.winner {
                // A little drawn crown, since Plex Mono has no ★ glyph.
                draw_crown(img, mid - 62, by0 - 20);
                draw_text(
                    img,
                    &fonts.bold,
                    15.0,
                    (mid + 8, by0 - 8),
                    GOLD,
                    Align::Center,
                    "WINNER",
                );
            }
        }
        None => {
            draw_text(
                img,
                &fonts.regular,
                20.0,
                (mid, (by0 + by1) / 2 + 7),
                INK,
                Align::Center,
                "not rated",
            );
        }
    }
}

/// A tiny gold crown (three merlons on a base bar) with a 1px ink shadow, since
/// the font has no star glyph. Top-left anchored, ~22x15.
fn draw_crown(img: &mut RgbImage, x: i64, y: i64) {
    let posts = [(x, y + 3), (x + 9, y), (x + 18, y + 3)];
    // Shadow pass, then gold, so it lifts off the green.
    for (colour, dx, dy) in [(INK, 1, 1), (GOLD, 0, 0)] {
        for &(px, py) in &posts {
            fill(img, px + dx, py + dy, px + dx + 3, y + dy + 12, colour);
        }
        fill(img, x + dx, y + dy + 9, x + 21 + dx, y + dy + 13, colour);
    }
}

/// The model name plated along the centre divider at the panel's bottom, over a
/// dark scrim so it reads on any coaster.
fn draw_name_plate(
    img: &mut RgbImage,
    fonts: &Fonts,
    panel_x0: i64,
    panel_y1: i64,
    panel_x1: i64,
    model: &str,
    is_left: bool,
) {
    let label = model.to_uppercase();
    const SIZE: f32 = 26.0;
    const PAD: i64 = 14;
    const M: i64 = 14;
    let tw = text_width(&fonts.bold, SIZE, &label) as i64;
    let py1 = panel_y1 - M;
    let py0 = py1 - 44;
    // Names hug the divider: right panel's plate on its left edge, and vice
    // versa, so the two names meet in the middle.
    let (px0, px1, anchor, align) = if is_left {
        let x1 = panel_x1 - M;
        (x1 - tw - 2 * PAD, x1, x1 - PAD, Align::Right)
    } else {
        let x0 = panel_x0 + M;
        (x0, x0 + tw + 2 * PAD, x0 + PAD, Align::Left)
    };
    blend_rect(img, px0, py0, px1, py1, INK, 0.62);
    fill(img, px0, py0, px1, py0, GOLD);
    draw_text(
        img,
        &fonts.bold,
        SIZE,
        (anchor, py1 - 13),
        CREAM,
        align,
        &label,
    );
}

/// Cards are JPEG: as PNG the set came to 98 MB of a deploy that only Slack's
/// and Twitter's crawlers ever fetch.
fn save_card(card: RgbImage, dest: &Path) -> Result<()> {
    let mut file = std::io::BufWriter::new(
        std::fs::File::create(dest).with_context(|| format!("writing {}", dest.display()))?,
    );
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 86)
        .encode_image(&card)
        .with_context(|| format!("encoding {}", dest.display()))
}

/// Writes a <=280px thumbnail of `shot` at `dest`, preserving aspect ratio.
/// Derives every display copy in parallel, before any page is rendered:
/// decoding 48-megapixel PNGs is the cost, and it parallelises perfectly.
pub fn derive_all(jobs: &[(std::path::PathBuf, std::path::PathBuf, u32)]) -> Result<()> {
    use rayon::prelude::*;
    jobs.par_iter()
        .try_for_each(|(src, dest, max_width)| write_poster(src, dest, *max_width))
}

/// A downscaled JPEG copy for display. A poster is fetched eagerly even when
/// the video is `preload="none"`, and the stills are up to 5536px of PNG.
pub fn write_poster(shot: &Path, dest: &Path, max_width: u32) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Cached across builds, which clean `assets/`.
    if let Some(cached) = derived_cache(shot, dest, max_width)
        && cached.is_file()
    {
        std::fs::copy(&cached, dest)?;
        return Ok(());
    }
    let img = image::open(shot).with_context(|| format!("reading {}", shot.display()))?;
    let img = if img.width() > max_width {
        // Triangle, not Lanczos3: indistinguishable at a 5x reduction of a
        // pixel-art render, and far cheaper.
        img.resize(max_width, u32::MAX, FilterType::Triangle)
    } else {
        img
    };
    let mut file = std::io::BufWriter::new(
        std::fs::File::create(dest).with_context(|| format!("writing {}", dest.display()))?,
    );
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 82);
    encoder
        .encode_image(&img.to_rgb8())
        .with_context(|| format!("encoding {}", dest.display()))?;
    drop(file);
    if let Some(cached) = derived_cache(shot, dest, max_width) {
        if let Some(parent) = cached.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::copy(dest, &cached);
    }
    Ok(())
}

/// Keyed on the source's path, size and mtime, so an edited capture misses.
fn derived_cache(shot: &Path, dest: &Path, max_width: u32) -> Option<std::path::PathBuf> {
    let meta = std::fs::metadata(shot).ok()?;
    let stamp = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(
        &(shot.to_string_lossy(), meta.len(), stamp, max_width),
        &mut hash,
    );
    let key = std::hash::Hasher::finish(&hash);
    let ext = dest.extension().and_then(|e| e.to_str()).unwrap_or("jpg");
    Some(
        std::env::temp_dir()
            .join("coaster-site-derived")
            .join(format!("{key:x}.{ext}")),
    )
}

/// The leaderboard's row thumbnail. JPEG: as PNG these cost 2.3 MB an index.
pub fn write_thumbnail(shot: &Path, dest: &Path) -> Result<()> {
    write_poster(shot, dest, 280)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_crop_fills_the_target_exactly() {
        let src = DynamicImage::ImageRgb8(RgbImage::from_pixel(100, 50, SKY));
        let out = cover_crop(&src, 60, 40);
        assert_eq!((out.width(), out.height()), (60, 40));
        let up = cover_crop(&src, 400, 400);
        assert_eq!((up.width(), up.height()), (400, 400));
    }

    #[test]
    fn outline_draws_only_the_border() {
        let mut img = RgbImage::from_pixel(10, 10, PANEL);
        outline(&mut img, 0, 0, 9, 9, 2, INK);
        assert_eq!(img.get_pixel(0, 0), &INK);
        assert_eq!(img.get_pixel(1, 1), &INK);
        assert_eq!(img.get_pixel(2, 2), &PANEL);
        assert_eq!(img.get_pixel(9, 9), &INK);
    }

    #[test]
    fn embedded_fonts_parse() {
        Fonts::load().expect("fonts should parse");
    }

    #[test]
    fn monospace_width_scales_with_length() {
        let fonts = Fonts::load().unwrap();
        let one = text_width(&fonts.bold, 30.0, "8");
        let four = text_width(&fonts.bold, 30.0, "8888");
        assert!(one > 0.0);
        assert!((four - one * 4.0).abs() < 0.5, "Plex Mono is monospace");
    }

    #[test]
    fn compare_card_is_the_right_size_and_has_ink() {
        let dir = std::env::temp_dir().join("coaster-site-card-test");
        std::fs::create_dir_all(&dir).unwrap();
        let shot = dir.join("shot.png");
        // A plain green tile stands in for a coaster capture.
        DynamicImage::ImageRgb8(RgbImage::from_pixel(280, 180, Rgb([0x3a, 0x6b, 0x2f])))
            .save(&shot)
            .unwrap();
        let dest = dir.join("card.jpg");
        write_compare_card(
            "Twister Roller Coaster",
            "design",
            &CardSide {
                shot: &shot,
                model: "claude-opus-4-8",
                score: Some(9.12),
                winner: true,
            },
            &CardSide {
                shot: &shot,
                model: "claude-sonnet-5",
                score: Some(7.44),
                winner: false,
            },
            &dest,
        )
        .unwrap();
        let out = image::open(&dest).unwrap();
        assert_eq!((out.width(), out.height()), (OG_W, OG_H));
        // The ribbon and text mean at least some ink-dark pixels exist over the
        // otherwise green/tan card.
        let has_ink = out
            .to_rgb8()
            .pixels()
            .any(|p| p.0[0] < 0x40 && p.0[1] < 0x40 && p.0[2] < 0x40);
        assert!(has_ink, "expected titlebar/text ink on the card");
    }
}
