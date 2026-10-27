//! Turn the pure badge pixels into a Win32 `HICON`.
//!
//! The icon owns a 32-bit top-down DIB (BGRA) and an all-zero monochrome mask;
//! [`CreateIconIndirect`] copies both into the icon, so the DIB and mask are
//! deleted before returning. The caller owns the returned icon and must destroy
//! it with [`DestroyIcon`].

#![expect(unsafe_code, reason = "raw GDI icon construction for the tray badge")]

use std::ptr;

use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, HICON, ICONINFO,
};

/// Build an `HICON` from straight-alpha RGBA at `size`×`size`.
///
/// Returns `None` for a wrong-sized buffer or any GDI failure; the caller falls
/// back to the brand glyph.
pub(crate) fn create_icon(rgba: &[u8], size: u32) -> Option<HICON> {
    let expected = (size as usize).checked_mul(size as usize)?.checked_mul(4)?;
    if size == 0 || rgba.len() != expected {
        return None;
    }

    let width = i32::try_from(size).ok()?;
    let height = i32::try_from(size).ok()?;
    let mask_stride = ((size as usize).div_ceil(16)) * 2;
    let mut mask_bits = vec![0u8; mask_stride * size as usize];

    // SAFETY: every pointer below points at a live local for the duration of
    // its call; `bits` is the DIB's writable pixel buffer.
    unsafe {
        let header = BITMAPINFOHEADER {
            biSize: u32::try_from(std::mem::size_of::<BITMAPINFOHEADER>()).ok()?,
            biWidth: width,
            // Negative height: top-down rows, matching the renderer's layout.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let info = BITMAPINFO {
            bmiHeader: header,
            bmiColors: [std::mem::zeroed()],
        };
        let mut bits: *mut core::ffi::c_void = ptr::null_mut();
        let color = CreateDIBSection(
            ptr::null_mut(),
            &raw const info,
            DIB_RGB_COLORS,
            &raw mut bits,
            ptr::null_mut(),
            0,
        );
        if color.is_null() || bits.is_null() {
            return None;
        }
        // SAFETY: CreateDIBSection handed us a writable buffer of exactly
        // `expected` bytes (32bpp, size×size).
        let destination = std::slice::from_raw_parts_mut(bits.cast::<u8>(), expected);
        copy_premultiplied(rgba, destination);

        let mask = CreateBitmap(width, height, 1, 1, mask_bits.as_mut_ptr().cast());
        if mask.is_null() {
            DeleteObject(color);
            return None;
        }

        let icon_info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&raw const icon_info);
        // CreateIconIndirect copies both bitmaps; ours are not part of the icon.
        DeleteObject(color);
        DeleteObject(mask);
        (!icon.is_null()).then_some(icon)
    }
}

/// Write BGRA pixels into the DIB, premultiplying alpha so the shell never
/// shows dark fringes around the anti-aliased disc edge.
fn copy_premultiplied(rgba: &[u8], destination: &mut [u8]) {
    let (sources, _) = rgba.as_chunks::<4>();
    let (targets, _) = destination.as_chunks_mut::<4>();
    for (source, target) in sources.iter().zip(targets.iter_mut()) {
        let alpha = u32::from(source[3]);
        target[0] = u8::try_from(u32::from(source[2]) * alpha / 255).unwrap_or(0);
        target[1] = u8::try_from(u32::from(source[1]) * alpha / 255).unwrap_or(0);
        target[2] = u8::try_from(u32::from(source[0]) * alpha / 255).unwrap_or(0);
        target[3] = source[3];
    }
}

/// Destroy an icon this module created.
pub(crate) fn destroy_icon(icon: HICON) {
    // SAFETY: the caller passes a handle from `create_icon` (or the PNG brand
    // helper) exactly once; DestroyIcon accepts icon handles only.
    unsafe {
        DestroyIcon(icon);
    }
}

#[cfg(test)]
mod tests {
    use super::super::badge::{Badge, BadgeText, BadgeTone, render};
    use super::{create_icon, destroy_icon};
    use openlogi_core::config::TrayBadgeStyle;

    #[test]
    fn every_tray_size_builds_an_icon() {
        for size in [16, 20, 24, 32] {
            let rgba = render(
                Badge {
                    text: BadgeText::Percent(87),
                    tone: BadgeTone::Accent,
                },
                size,
                false,
                TrayBadgeStyle::Circle,
            );
            let icon = create_icon(&rgba, size).expect("badge icon");
            destroy_icon(icon);
        }
    }

    #[test]
    fn a_wrong_sized_buffer_is_rejected() {
        assert!(create_icon(&[0; 16], 16).is_none());
    }
}
