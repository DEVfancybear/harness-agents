//! A manual check of the desktop calls on a real screen, no model involved:
//!
//!     cargo run -p harness-desktop --example desktop_probe
//!
//! It captures the primary display and reports its size and how many pixels are
//! not black (a locked or blank capture is all black), then moves the pointer 40
//! pixels right and down, reads it back, and puts it where it was.

fn main() -> Result<(), harness_desktop::Error> {
    let (width, height) = harness_desktop::screen_size()?;
    println!("screen {width}x{height}");
    let frame = harness_desktop::capture()?;
    let lit = frame
        .rgba
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 8 || pixel[1] > 8 || pixel[2] > 8)
        .count();
    println!(
        "capture {}x{}, {lit} of {} pixels not black",
        frame.width,
        frame.height,
        frame.rgba.len() / 4
    );
    let start = harness_desktop::cursor_position()?;
    let target = (start.0 + 40, start.1 + 40);
    harness_desktop::move_pointer(target.0, target.1)?;
    let landed = harness_desktop::cursor_position()?;
    harness_desktop::move_pointer(start.0, start.1)?;
    println!("pointer {start:?} -> asked {target:?}, landed {landed:?}");
    println!("keyboard window: {:?}", harness_desktop::foreground_title());
    Ok(())
}
