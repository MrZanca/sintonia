//! Immagini per Slint: loghi così come sono, locandine ritagliate a 2:3 con gli angoli a
//! super-ellisse già trasparenti.
//!
//! Slint non sa ritagliare un'immagine con un percorso, e gli angoli devono essere quelli del
//! componente `Sq` (non archi di cerchio). Quindi il ritaglio si fa qui, sui pixel, con la stessa
//! curva: angolo di Bézier cubica da (0,p) a (p,0) con controlli a p/4 dall'angolo.

use sintonia_core::service::LogoImage;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// Proporzioni delle locandine (larghezza / altezza).
const POSTER_RATIO: f32 = 2.0 / 3.0;
/// Raggio degli angoli in rapporto alla larghezza: `Sq { r: 18px }` su una locandina larga
/// 150 px, con p = 1,5 r.
const POSTER_CORNER: f32 = 18.0 * 1.5 / 150.0;

/// Un logo, senza modifiche.
pub fn logo(image: LogoImage) -> Image {
    let buffer =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&image.rgba, image.width, image.height);
    Image::from_rgba8(buffer)
}

/// Una locandina: ritaglio centrale a 2:3 e angoli trasparenti.
pub fn poster(image: LogoImage) -> Image {
    let (rgba, width, height) = crop_to_ratio(&image.rgba, image.width, image.height, POSTER_RATIO);
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, width, height);
    let corner = width as f32 * POSTER_CORNER;
    mask_corners(buffer.make_mut_bytes(), width, height, corner);
    Image::from_rgba8(buffer)
}

/// Ritaglio centrale alle proporzioni `ratio` (larghezza / altezza).
fn crop_to_ratio(rgba: &[u8], width: u32, height: u32, ratio: f32) -> (Vec<u8>, u32, u32) {
    if width == 0 || height == 0 {
        return (rgba.to_vec(), width, height);
    }
    let current = width as f32 / height as f32;
    let (new_w, new_h) = if current > ratio {
        (
            ((height as f32 * ratio).round() as u32).clamp(1, width),
            height,
        )
    } else {
        (
            width,
            ((width as f32 / ratio).round() as u32).clamp(1, height),
        )
    };
    let (x0, y0) = ((width - new_w) / 2, (height - new_h) / 2);
    let mut out = Vec::with_capacity((new_w * new_h * 4) as usize);
    for y in y0..y0 + new_h {
        let start = ((y * width + x0) * 4) as usize;
        out.extend_from_slice(&rgba[start..start + (new_w * 4) as usize]);
    }
    (out, new_w, new_h)
}

/// y della curva d'angolo per x = s (0 ≤ s ≤ p); x(t) cresce con t: bisezione.
fn curve(s: f32, p: f32) -> f32 {
    let q = 0.25 * p;
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..20 {
        let t = 0.5 * (lo + hi);
        let x = 3.0 * (1.0 - t) * t * t * q + t * t * t * p;
        if x < s {
            lo = t;
        } else {
            hi = t;
        }
    }
    let t = 0.5 * (lo + hi);
    let it = 1.0 - t;
    it * it * it * p + 3.0 * it * it * t * q
}

/// Copertura (0..1) del pixel con centro a distanza `u`, `v` dai due bordi dell'angolo più vicino.
fn coverage(u: f32, v: f32, p: f32) -> f32 {
    if p <= 0.0 || u >= p || v >= p {
        return 1.0;
    }
    // stessa approssimazione dello shader del video: distanze verticale e orizzontale dalla curva
    let dv = v - curve(u, p);
    let du = u - curve(v, p);
    let dist = dv.signum() * (dv * du).abs() / (dv * dv + du * du).sqrt().max(1e-4);
    (dist + 0.5).clamp(0.0, 1.0)
}

/// Rende trasparenti gli angoli (alfa moltiplicato per la copertura; Slint usa alfa non
/// premoltiplicato per queste immagini).
fn mask_corners(rgba: &mut [u8], width: u32, height: u32, corner: f32) {
    let p = corner.min(width.min(height) as f32 * 0.4);
    if p <= 0.0 {
        return;
    }
    let reach = p.ceil() as u32 + 1;
    for y in 0..height {
        let cy = y as f32 + 0.5;
        let v = cy.min(height as f32 - cy);
        if v >= p {
            continue;
        }
        for x in (0..width.min(reach)).chain(width.saturating_sub(reach).max(reach)..width) {
            let cx = x as f32 + 0.5;
            let u = cx.min(width as f32 - cx);
            let c = coverage(u, v, p);
            if c < 1.0 {
                let alpha = &mut rgba[((y * width + x) * 4 + 3) as usize];
                *alpha = (f32::from(*alpha) * c).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32) -> Vec<u8> {
        vec![255; (width * height * 4) as usize]
    }

    #[test]
    fn crop_keeps_the_center() {
        let (rgba, w, h) = crop_to_ratio(&solid(300, 200), 300, 200, POSTER_RATIO);
        assert_eq!((w, h), (133, 200));
        assert_eq!(rgba.len(), (133 * 200 * 4) as usize);
        let (_, w, h) = crop_to_ratio(&solid(200, 600), 200, 600, POSTER_RATIO);
        assert_eq!((w, h), (200, 300));
    }

    #[test]
    fn corners_become_transparent_and_center_stays() {
        let (w, h) = (120, 180);
        let mut rgba = solid(w, h);
        mask_corners(&mut rgba, w, h, 20.0);
        let alpha = |x: u32, y: u32| rgba[((y * w + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(w - 1, h - 1), 0);
        assert_eq!(alpha(w / 2, h / 2), 255);
        assert_eq!(alpha(w / 2, 0), 255, "bordo dritto pieno");
        assert_eq!(alpha(0, h / 2), 255);
        // simmetria tra gli angoli
        assert_eq!(alpha(3, 3), alpha(w - 4, h - 4));
    }

    #[test]
    fn curve_is_symmetric_with_the_shader() {
        let p = 30.0;
        assert!((curve(0.0, p) - p).abs() < 1e-3);
        assert!(curve(p, p).abs() < 1e-3);
        // a metà curva (t = 0,5) il punto sta sulla diagonale: x = y = 3/32 p + p/8
        let x = 3.0 / 32.0 * p + p / 8.0;
        assert!((curve(x, p) - x).abs() < 0.05);
    }
}
