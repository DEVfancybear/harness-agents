//! The Win32 calls behind the desktop: GDI for the capture, `SendInput` for input.
//!
//! The only module of the workspace allowed `unsafe`. Each block calls one Win32
//! function with handles this module owns (released by a guard on every path) or
//! plain structs it built, and says so.
#![allow(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "Win32 takes screen sizes as i32, flags and wheel deltas as u32 and UTF-16 units as u16; every value converted here is a pixel count, a small key code or a delta that fits"
)]

use std::{mem::size_of, ptr::null_mut};

use windows_sys::Win32::{
    Foundation::POINT,
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
        CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HBITMAP, HDC,
        HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
    },
    UI::{
        HiDpi::{
            DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            SetThreadDpiAwarenessContext,
        },
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
            KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MOUSEEVENTF_ABSOLUTE,
            MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
            MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
            MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, MapVirtualKeyW, SendInput,
            VkKeyScanW,
        },
        WindowsAndMessaging::{
            CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetCursorPos,
            GetForegroundWindow, GetIconInfo, GetSystemMetrics, GetWindowTextW, ICONINFO,
            SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_SWAPBUTTON,
            SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SetCursorPos,
        },
    },
};

use crate::{Button, Error, Frame, Key, Result};

/// The calling thread sees physical pixels while this lives, and its earlier
/// awareness comes back after. Per-thread, so the rest of the process (the TUI)
/// keeps whatever it had.
struct PhysicalPixels(DPI_AWARENESS_CONTEXT);

impl PhysicalPixels {
    fn enter() -> Self {
        // SAFETY: takes a predefined context constant and changes only this thread's
        // awareness; a null return (older Windows) leaves the thread as it was.
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for PhysicalPixels {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: restores the context the same thread had before `enter`.
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

fn metric(index: i32) -> i32 {
    // SAFETY: reads one system metric; no pointers.
    unsafe { GetSystemMetrics(index) }
}

pub fn screen_size() -> Result<(u32, u32)> {
    let _pixels = PhysicalPixels::enter();
    primary_size()
}

fn primary_size() -> Result<(u32, u32)> {
    let (width, height) = (metric(SM_CXSCREEN), metric(SM_CYSCREEN));
    if width <= 0 || height <= 0 {
        return Err(Error::Failed(
            "Windows reports no primary display (a service or a disconnected session has no desktop)"
                .to_owned(),
        ));
    }
    Ok((width as u32, height as u32))
}

/// GDI objects released in reverse order on every path out of `capture`.
struct Gdi {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Drop for Gdi {
    fn drop(&mut self) {
        // SAFETY: each handle was created by `capture` and is released exactly once;
        // null handles (a step that failed) are skipped.
        unsafe {
            if !self.previous.is_null() {
                SelectObject(self.memory, self.previous);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            if !self.memory.is_null() {
                DeleteDC(self.memory);
            }
            if !self.screen.is_null() {
                ReleaseDC(null_mut(), self.screen);
            }
        }
    }
}

fn locked() -> Error {
    Error::Failed(
        "the screen could not be captured: the desktop is locked, asleep or on the secure desktop (a UAC prompt); unlock it and try again"
            .to_owned(),
    )
}

pub fn capture() -> Result<Frame> {
    let _pixels = PhysicalPixels::enter();
    let (width, height) = primary_size()?;
    let (w, h) = (width as i32, height as i32);
    let mut gdi = Gdi {
        screen: null_mut(),
        memory: null_mut(),
        bitmap: null_mut(),
        previous: null_mut(),
    };
    // SAFETY: the screen DC, the memory DC and its bitmap are owned by `gdi`, which
    // releases them; BitBlt copies between the two DCs inside their sizes.
    unsafe {
        gdi.screen = GetDC(null_mut());
        if gdi.screen.is_null() {
            return Err(locked());
        }
        gdi.memory = CreateCompatibleDC(gdi.screen);
        gdi.bitmap = CreateCompatibleBitmap(gdi.screen, w, h);
        if gdi.memory.is_null() || gdi.bitmap.is_null() {
            return Err(locked());
        }
        gdi.previous = SelectObject(gdi.memory, gdi.bitmap);
        if BitBlt(
            gdi.memory,
            0,
            0,
            w,
            h,
            gdi.screen,
            0,
            0,
            SRCCOPY | CAPTUREBLT,
        ) == 0
        {
            return Err(locked());
        }
    }
    draw_pointer(gdi.memory);
    // SAFETY: deselects the bitmap so GetDIBits may read it, as GDI requires.
    unsafe {
        SelectObject(gdi.memory, gdi.previous);
    }
    gdi.previous = null_mut();
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Negative: rows top first.
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };
    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    // SAFETY: `pixels` holds exactly `height` rows of `width` 32-bit pixels, the
    // layout `info` asks for.
    let rows = unsafe {
        GetDIBits(
            gdi.memory,
            gdi.bitmap,
            0,
            height,
            pixels.as_mut_ptr().cast(),
            &raw mut info,
            DIB_RGB_COLORS,
        )
    };
    drop(gdi);
    if rows != h {
        return Err(locked());
    }
    // GDI hands back BGRX; the encoder wants RGBA, opaque.
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    Ok(Frame {
        width,
        height,
        rgba: pixels,
    })
}

/// Draw the pointer into the capture where it is, as the person at the screen
/// sees it: a model that cannot see the pointer cannot tell where it will click.
fn draw_pointer(memory: HDC) {
    let mut cursor = CURSORINFO {
        cbSize: size_of::<CURSORINFO>() as u32,
        ..CURSORINFO::default()
    };
    // SAFETY: `cursor` is sized as the call requires; the icon's bitmaps that
    // GetIconInfo creates are deleted here.
    unsafe {
        if GetCursorInfo(&raw mut cursor) == 0
            || cursor.flags & CURSOR_SHOWING == 0
            || cursor.hCursor.is_null()
        {
            return;
        }
        let mut icon = ICONINFO::default();
        let (hot_x, hot_y) = if GetIconInfo(cursor.hCursor, &raw mut icon) == 0 {
            (0, 0)
        } else {
            if !icon.hbmMask.is_null() {
                DeleteObject(icon.hbmMask);
            }
            if !icon.hbmColor.is_null() {
                DeleteObject(icon.hbmColor);
            }
            (icon.xHotspot as i32, icon.yHotspot as i32)
        };
        DrawIconEx(
            memory,
            cursor.ptScreenPos.x - hot_x,
            cursor.ptScreenPos.y - hot_y,
            cursor.hCursor,
            0,
            0,
            0,
            null_mut(),
            DI_NORMAL,
        );
    }
}

pub fn cursor_position() -> Result<(i32, i32)> {
    let _pixels = PhysicalPixels::enter();
    pointer()
}

fn pointer() -> Result<(i32, i32)> {
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: writes one POINT this function owns.
    if unsafe { GetCursorPos(&raw mut point) } == 0 {
        return Err(Error::Failed(
            "the pointer position could not be read: the desktop is locked or not interactive"
                .to_owned(),
        ));
    }
    Ok((point.x, point.y))
}

fn send(inputs: &[INPUT]) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    // SAFETY: the slice is a contiguous array of initialized INPUT structs.
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "Windows took {sent} of {} input events: input is blocked (the desktop is locked, a UAC prompt is up, or the focused window runs as administrator)",
            inputs.len()
        )))
    }
}

fn mouse(dx: i32, dy: i32, data: u32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn keyboard(code: u16, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: code,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// A pixel of the virtual desktop in `SendInput`'s 0..=65535 absolute units: the
/// first pixel is 0 and the last is 65535.
fn normalized(pixel: i32, origin: i32, size: i32) -> i32 {
    let span = i64::from(size.max(2) - 1);
    let offset = i64::from(pixel - origin).clamp(0, span);
    ((offset * 65_535 + span / 2) / span) as i32
}

pub fn move_pointer(x: i32, y: i32) -> Result<()> {
    let _pixels = PhysicalPixels::enter();
    let (vx, vy) = (metric(SM_XVIRTUALSCREEN), metric(SM_YVIRTUALSCREEN));
    let (vw, vh) = (metric(SM_CXVIRTUALSCREEN), metric(SM_CYVIRTUALSCREEN));
    send(&[mouse(
        normalized(x, vx, vw),
        normalized(y, vy, vh),
        0,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    )])?;
    // The 0..65535 grid can land a pixel off on a large desktop; set it exactly.
    if pointer()? != (x, y) {
        // SAFETY: moves the pointer; no pointers passed.
        unsafe { SetCursorPos(x, y) };
    }
    Ok(())
}

pub fn button(button: Button, down: bool) -> Result<()> {
    let _pixels = PhysicalPixels::enter();
    // `SendInput` names physical buttons: with the buttons swapped for a left hand,
    // the primary (left) button is the physical right one.
    let swapped = metric(SM_SWAPBUTTON) != 0;
    let flags = match (button, swapped, down) {
        (Button::Left, false, true) | (Button::Right, true, true) => MOUSEEVENTF_LEFTDOWN,
        (Button::Left, false, false) | (Button::Right, true, false) => MOUSEEVENTF_LEFTUP,
        (Button::Right, false, true) | (Button::Left, true, true) => MOUSEEVENTF_RIGHTDOWN,
        (Button::Right, false, false) | (Button::Left, true, false) => MOUSEEVENTF_RIGHTUP,
        (Button::Middle, _, true) => MOUSEEVENTF_MIDDLEDOWN,
        (Button::Middle, _, false) => MOUSEEVENTF_MIDDLEUP,
    };
    send(&[mouse(0, 0, 0, flags)])
}

pub fn scroll(notches: i32, horizontal: bool) -> Result<()> {
    let _pixels = PhysicalPixels::enter();
    // One notch is WHEEL_DELTA (120). Positive turns the wheel away from the user
    // (up); the horizontal wheel's positive is right.
    let flag = if horizontal {
        MOUSEEVENTF_HWHEEL
    } else {
        MOUSEEVENTF_WHEEL
    };
    send(&[mouse(0, 0, (notches * 120) as u32, flag)])
}

fn scan_code(code: u16) -> u16 {
    // SAFETY: a pure lookup in the keyboard layout.
    unsafe { MapVirtualKeyW(u32::from(code), MAPVK_VK_TO_VSC) as u16 }
}

fn key_event(code: u16, extended: bool, down: bool) -> INPUT {
    let mut flags = if down { 0 } else { KEYEVENTF_KEYUP };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    keyboard(code, scan_code(code), flags)
}

/// The virtual key the layout presses for a character, and the shift state it
/// needs (1 shift, 2 ctrl, 4 alt), or `None` when the layout has no key for it.
fn layout_key(character: char) -> Option<(u16, u8)> {
    let mut units = [0_u16; 2];
    let [unit] = character.encode_utf16(&mut units) else {
        return None;
    };
    // SAFETY: a pure lookup in the keyboard layout.
    let scanned = unsafe { VkKeyScanW(*unit) };
    if scanned == -1 {
        return None;
    }
    let scanned = scanned as u16;
    Some((scanned & 0xFF, (scanned >> 8) as u8))
}

pub fn key(key: Key, down: bool) -> Result<()> {
    let (code, extended) = match key {
        Key::Virtual { code, extended } => (code, extended),
        Key::Char(character) => {
            let (code, _) = layout_key(character).ok_or_else(|| {
                Error::Failed(format!(
                    "the keyboard layout has no key for {character:?}; type it as text instead"
                ))
            })?;
            (code, false)
        }
    };
    send(&[key_event(code, extended, down)])
}

const VK_SHIFT: u16 = 0x10;

pub fn type_char(character: char) -> Result<()> {
    let inputs = match character {
        '\n' => vec![key_event(0x0D, false, true), key_event(0x0D, false, false)],
        '\t' => vec![key_event(0x09, false, true), key_event(0x09, false, false)],
        other => unicode(other),
    };
    send(&inputs)
}

pub fn press_char(character: char) -> Result<()> {
    let (code, shift) = layout_key(character)
        // Shift at most: a key that needs Ctrl or AltGr would fire shortcuts.
        .filter(|(_, shift)| shift & !1 == 0)
        .ok_or_else(|| {
            Error::Failed(format!(
                "the keyboard layout has no plain key for {character:?}; type it as text instead"
            ))
        })?;
    let mut inputs = Vec::with_capacity(4);
    if shift == 1 {
        inputs.push(key_event(VK_SHIFT, false, true));
    }
    inputs.push(key_event(code, false, true));
    inputs.push(key_event(code, false, false));
    if shift == 1 {
        inputs.push(key_event(VK_SHIFT, false, false));
    }
    send(&inputs)
}

/// A character as Unicode input: its UTF-16 units pressed, then released.
fn unicode(character: char) -> Vec<INPUT> {
    let mut units = [0_u16; 2];
    let units = character.encode_utf16(&mut units);
    let mut inputs: Vec<INPUT> = units
        .iter()
        .map(|unit| keyboard(0, *unit, KEYEVENTF_UNICODE))
        .collect();
    inputs.extend(
        units
            .iter()
            .map(|unit| keyboard(0, *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)),
    );
    inputs
}

pub fn foreground_title() -> Option<String> {
    // SAFETY: reads the foreground window's title into a buffer this function owns,
    // at most its length.
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }
        let mut buffer = [0_u16; 512];
        let length = GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32);
        let length = usize::try_from(length).ok().filter(|length| *length > 0)?;
        Some(String::from_utf16_lossy(&buffer[..length]))
    }
}

#[cfg(test)]
mod tests {
    use super::normalized;

    #[test]
    fn absolute_units_span_the_desktop_end_to_end() {
        assert_eq!(normalized(0, 0, 1920), 0);
        assert_eq!(normalized(1919, 0, 1920), 65_535);
        assert_eq!(normalized(960, 0, 1920), 32_785);
        // A second monitor left of the primary moves the origin.
        assert_eq!(normalized(-1920, -1920, 3840), 0);
        assert_eq!(normalized(5000, 0, 1920), 65_535);
    }
}
