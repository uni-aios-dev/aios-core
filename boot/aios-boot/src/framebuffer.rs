//! GOP framebuffer detection.
//!
//! The firmware reports a physical framebuffer address; the kernel reaches it
//! through the higher-half direct map, so only the description is handed over.

use aios_bootinfo::FramebufferInfo;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};

/// Returns the current GOP mode as a [`FramebufferInfo`], or `None` when the
/// firmware has no usable linear framebuffer.
pub fn detect() -> Option<FramebufferInfo> {
    let handle = uefi::boot::get_handle_for_protocol::<GraphicsOutput>().ok()?;
    let mut gop = uefi::boot::open_protocol_exclusive::<GraphicsOutput>(handle).ok()?;

    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let stride = mode.stride();
    let address = gop.frame_buffer().as_mut_ptr() as u64;
    if width == 0 || height == 0 || stride == 0 || address == 0 {
        return None;
    }

    let (bpp, red, green, blue) = match mode.pixel_format() {
        PixelFormat::Rgb => (32u16, 0x0000_00ffu32, 0x0000_ff00u32, 0x00ff_0000u32),
        PixelFormat::Bgr => (32u16, 0x00ff_0000u32, 0x0000_ff00u32, 0x0000_00ffu32),
        PixelFormat::Bitmask => {
            let mask = mode.pixel_bitmask()?;
            let used = mask.red | mask.green | mask.blue | mask.reserved;
            if used == 0 {
                return None;
            }
            let bits = 32 - used.leading_zeros();
            let bpp = (bits.div_ceil(8) * 8) as u16;
            (bpp, mask.red, mask.green, mask.blue)
        }
        PixelFormat::BltOnly => return None,
    };

    let pitch = stride as u64 * u64::from(bpp / 8);
    let pitch = u32::try_from(pitch).ok()?;
    let (red_shift, red_size) = channel(red);
    let (green_shift, green_size) = channel(green);
    let (blue_shift, blue_size) = channel(blue);

    Some(FramebufferInfo {
        address,
        width: width as u32,
        height: height as u32,
        pitch,
        bpp,
        red_shift,
        red_size,
        green_shift,
        green_size,
        blue_shift,
        blue_size,
        reserved: 0,
    })
}

fn channel(mask: u32) -> (u8, u8) {
    if mask == 0 {
        (0, 0)
    } else {
        (mask.trailing_zeros() as u8, mask.count_ones() as u8)
    }
}
