//! A small Windows-like desktop: gradient wallpaper, two draggable windows,
//! a start menu and a taskbar. Rendering is done directly into the framebuffer.
//!
//! The cursor is handled with a classic save-under buffer: before the pointer
//! is painted the covered pixels are copied out, and the next cursor move
//! restores them first. Full desktop redraws ignore the old cursor entirely,
//! so no streaking can occur.

use bootloader_api::info::{FrameBufferInfo, PixelFormat};

use crate::input::MouseState;

const TASKBAR_H: usize = 36;
const TITLEBAR_H: usize = 28;
const CLOSE_SIZE: usize = 18;
const WINDOW_COUNT: usize = 2;
const MENU_W: usize = 226;
const MENU_H: usize = 96;
const MENU_ITEM_H: usize = 32;
const TASKBAR_BTN_W: usize = 176;
const MIN_VISIBLE: isize = 48;

const CURSOR_W: usize = 16;
const CURSOR_H: usize = 20;

#[derive(Clone, Copy)]
struct Rgb(u8, u8, u8);

const DESKTOP_TOP: Rgb = Rgb(12, 46, 94);
const DESKTOP_BOTTOM: Rgb = Rgb(46, 124, 190);
const DESKTOP_MARK: Rgb = Rgb(78, 142, 196);

const TASKBAR_BG: Rgb = Rgb(22, 29, 40);
const TASKBAR_ACCENT: Rgb = Rgb(58, 122, 188);

const START_IDLE: Rgb = Rgb(44, 126, 68);
const START_HOVER: Rgb = Rgb(60, 156, 86);
const START_ACTIVE: Rgb = Rgb(78, 184, 104);

const TITLE_ACTIVE: Rgb = Rgb(30, 92, 168);
const TITLE_INACTIVE: Rgb = Rgb(112, 128, 146);

const WIN_BG: Rgb = Rgb(243, 246, 250);
const WIN_BORDER: Rgb = Rgb(34, 46, 60);
const SHADOW: Rgb = Rgb(18, 30, 48);
const SEPARATOR: Rgb = Rgb(206, 215, 226);

const CLOSE_BG: Rgb = Rgb(196, 54, 54);
const CLOSE_HOVER: Rgb = Rgb(228, 82, 82);
const BTN_FACE: Rgb = Rgb(214, 223, 233);
const BTN_HOVER: Rgb = Rgb(190, 205, 221);

const TEXT_DARK: Rgb = Rgb(22, 28, 36);
const TEXT_LIGHT: Rgb = Rgb(250, 252, 255);
const TEXT_MUTED: Rgb = Rgb(120, 134, 150);
const ACCENT: Rgb = Rgb(30, 92, 168);

const ICON_BG: Rgb = Rgb(238, 246, 255);
const ICON_FG: Rgb = Rgb(30, 92, 168);
const CURSOR_FILL: Rgb = Rgb(255, 255, 255);
const CURSOR_OUTLINE: Rgb = Rgb(8, 12, 20);

const CURSOR_SHAPE: [&str; 18] = [
    "X...............",
    "XX..............",
    "X.X.............",
    "X..X............",
    "X...X...........",
    "X....X..........",
    "X.....X.........",
    "X......X........",
    "X.......X.......",
    "X........X......",
    "X.....XXXXX.....",
    "X..X..X.........",
    "X.X.X..X........",
    "XX..X..X........",
    "X....X..X.......",
    ".....X..X.......",
    "......XX........",
    "................",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowKind {
    Welcome,
    Counter,
}

#[derive(Clone, Copy)]
struct Window {
    title: &'static str,
    x: isize,
    y: isize,
    w: usize,
    h: usize,
    open: bool,
    kind: WindowKind,
}

#[derive(Clone, Copy)]
struct Rect {
    x: isize,
    y: isize,
    w: usize,
    h: usize,
}

impl Rect {
    fn contains(self, mx: usize, my: usize) -> bool {
        let x = mx as isize;
        let y = my as isize;
        x >= self.x
            && x < self.x + self.w as isize
            && y >= self.y
            && y < self.y + self.h as isize
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hover {
    None,
    Menu,
    Task(usize),
    StartItem(usize),
    Close(usize),
    Counter,
}

#[derive(Clone, Copy)]
struct Drag {
    idx: usize,
    off_x: isize,
    off_y: isize,
}

pub struct Desktop {
    width: usize,
    height: usize,
    stride: usize,
    bpp: usize,
    format: PixelFormat,
    windows: [Window; WINDOW_COUNT],
    z_order: [usize; WINDOW_COUNT],
    drag: Option<Drag>,
    menu_open: bool,
    clicks: u32,
    hover: Hover,
    cursor_active: bool,
    cursor_x: usize,
    cursor_y: usize,
    cursor_saved_w: usize,
    cursor_saved_h: usize,
    cursor_saved: [u8; CURSOR_W * CURSOR_H * 4],
}

impl Desktop {
    pub fn new(info: FrameBufferInfo) -> Self {
        let width = info.width.max(1);
        let height = info.height.max(1);

        let welcome_w = width.saturating_sub(40).min(440);
        let counter_w = width.saturating_sub(40).min(396);
        let welcome_h = 236;
        let counter_h = 236;

        let max_y = height.saturating_sub(TASKBAR_H + 20 + welcome_h) as isize;
        let welcome_y = 70.min(max_y.max(8));
        let counter_y = 150.min(max_y.max(8));

        let welcome_x = (width as isize / 5).max(20);
        let counter_x = (width as isize / 2)
            .min(width as isize - counter_w as isize - 20)
            .max(20);

        Self {
            width,
            height,
            stride: info.stride,
            bpp: info.bytes_per_pixel,
            format: info.pixel_format,
            windows: [
                Window {
                    title: "Welcome",
                    x: welcome_x,
                    y: welcome_y,
                    w: welcome_w,
                    h: welcome_h,
                    open: true,
                    kind: WindowKind::Welcome,
                },
                Window {
                    title: "Click Counter",
                    x: counter_x,
                    y: counter_y,
                    w: counter_w,
                    h: counter_h,
                    open: true,
                    kind: WindowKind::Counter,
                },
            ],
            z_order: [0, 1],
            drag: None,
            menu_open: false,
            clicks: 0,
            hover: Hover::None,
            cursor_active: false,
            cursor_x: 0,
            cursor_y: 0,
            cursor_saved_w: 0,
            cursor_saved_h: 0,
            cursor_saved: [0; CURSOR_W * CURSOR_H * 4],
        }
    }

    // ------------------------------------------------------------------
    // Geometry helpers
    // ------------------------------------------------------------------

    fn taskbar_y(&self) -> isize {
        self.height as isize - TASKBAR_H as isize
    }

    fn window_rect(&self, idx: usize) -> Rect {
        let window = self.windows[idx];
        Rect {
            x: window.x,
            y: window.y,
            w: window.w,
            h: window.h,
        }
    }

    fn titlebar_rect(&self, idx: usize) -> Rect {
        let window = self.windows[idx];
        Rect {
            x: window.x,
            y: window.y,
            w: window.w,
            h: TITLEBAR_H,
        }
    }

    fn close_rect(&self, idx: usize) -> Rect {
        let window = self.windows[idx];
        Rect {
            x: window.x + window.w as isize - CLOSE_SIZE as isize - 6,
            y: window.y + (TITLEBAR_H - CLOSE_SIZE) as isize / 2,
            w: CLOSE_SIZE,
            h: CLOSE_SIZE,
        }
    }

    fn counter_button_rect(&self, idx: usize) -> Rect {
        let window = self.windows[idx];
        Rect {
            x: window.x + 26,
            y: window.y + 108,
            w: 190,
            h: 40,
        }
    }

    fn menu_button_rect(&self) -> Rect {
        Rect {
            x: 6,
            y: self.taskbar_y() + 5,
            w: 86,
            h: TASKBAR_H - 10,
        }
    }

    fn task_button_rect(&self, idx: usize) -> Rect {
        Rect {
            x: 108 + idx as isize * (TASKBAR_BTN_W as isize + 8),
            y: self.taskbar_y() + 5,
            w: TASKBAR_BTN_W,
            h: TASKBAR_H - 10,
        }
    }

    fn menu_rect(&self) -> Rect {
        Rect {
            x: 6,
            y: self.taskbar_y() - MENU_H as isize - 6,
            w: MENU_W,
            h: MENU_H,
        }
    }

    fn menu_item_rect(&self, idx: usize) -> Rect {
        let menu = self.menu_rect();
        Rect {
            x: menu.x + 8,
            y: menu.y + 18 + idx as isize * (MENU_ITEM_H as isize + 4),
            w: menu.w - 16,
            h: MENU_ITEM_H,
        }
    }

    // ------------------------------------------------------------------
    // Window state
    // ------------------------------------------------------------------

    fn active_window(&self) -> Option<usize> {
        self.z_order
            .iter()
            .rev()
            .copied()
            .find(|&idx| self.windows[idx].open)
    }

    fn bring_to_front(&mut self, idx: usize) {
        if self.z_order[WINDOW_COUNT - 1] != idx {
            let other = self.z_order[WINDOW_COUNT - 1];
            self.z_order = [other, idx];
        }
    }

    fn open_window(&mut self, idx: usize) {
        self.windows[idx].open = true;
        self.bring_to_front(idx);
        self.menu_open = false;
    }

    fn hover_at(&self, mx: usize, my: usize) -> Hover {
        if (my as isize) >= self.taskbar_y() {
            if self.menu_button_rect().contains(mx, my) {
                return Hover::Menu;
            }
            for idx in 0..WINDOW_COUNT {
                if self.task_button_rect(idx).contains(mx, my) {
                    return Hover::Task(idx);
                }
            }
            return Hover::None;
        }

        if self.menu_open {
            if self.menu_rect().contains(mx, my) {
                for idx in 0..WINDOW_COUNT {
                    if self.menu_item_rect(idx).contains(mx, my) {
                        return Hover::StartItem(idx);
                    }
                }
            }
            return Hover::None;
        }

        for &idx in self.z_order.iter().rev() {
            if !self.windows[idx].open || !self.window_rect(idx).contains(mx, my) {
                continue;
            }
            if self.close_rect(idx).contains(mx, my) {
                return Hover::Close(idx);
            }
            if self.windows[idx].kind == WindowKind::Counter
                && self.counter_button_rect(idx).contains(mx, my)
            {
                return Hover::Counter;
            }
            return Hover::None;
        }

        Hover::None
    }

    fn press(&mut self, mx: usize, my: usize) -> bool {
        if (my as isize) >= self.taskbar_y() {
            if self.menu_button_rect().contains(mx, my) {
                self.menu_open = !self.menu_open;
                self.drag = None;
                return true;
            }

            let menu_was_open = self.menu_open;
            self.menu_open = false;
            for idx in 0..WINDOW_COUNT {
                if self.task_button_rect(idx).contains(mx, my) {
                    self.open_window(idx);
                    return true;
                }
            }
            return menu_was_open;
        }

        if self.menu_open {
            if self.menu_rect().contains(mx, my) {
                for idx in 0..WINDOW_COUNT {
                    if self.menu_item_rect(idx).contains(mx, my) {
                        self.open_window(idx);
                        return true;
                    }
                }
            }
            self.menu_open = false;
            return true;
        }

        for &idx in self.z_order.iter().rev() {
            if !self.windows[idx].open || !self.window_rect(idx).contains(mx, my) {
                continue;
            }

            if self.close_rect(idx).contains(mx, my) {
                self.windows[idx].open = false;
                self.drag = None;
                return true;
            }

            if self.titlebar_rect(idx).contains(mx, my) {
                let window = self.windows[idx];
                self.bring_to_front(idx);
                self.drag = Some(Drag {
                    idx,
                    off_x: mx as isize - window.x,
                    off_y: my as isize - window.y,
                });
                return true;
            }

            if self.windows[idx].kind == WindowKind::Counter
                && self.counter_button_rect(idx).contains(mx, my)
            {
                self.bring_to_front(idx);
                self.clicks = self.clicks.wrapping_add(1);
                crate::serial_println!("CLICK COUNTER: count = {}", self.clicks);
                return true;
            }

            if self.active_window() != Some(idx) {
                self.bring_to_front(idx);
                return true;
            }
            return false;
        }

        false
    }

    /// Process one mouse update. Returns `true` when the whole desktop needs
    /// to be repainted; otherwise a simple cursor move is enough.
    pub fn on_mouse(&mut self, state: &MouseState) -> bool {
        let mut full_redraw = false;

        if state.left_released {
            crate::serial_println!("DESKTOP: release at ({},{})", state.x, state.y);
            if self.drag.take().is_some() {
                full_redraw = true;
            }
        }

        if let Some(drag) = self.drag {
            crate::serial_println!(
                "DESKTOP: drag window {} down={} to ({},{})",
                drag.idx,
                state.left_down,
                state.x,
                state.y
            );
            if state.left_down {
                let taskbar_y = self.taskbar_y();
                let window = &mut self.windows[drag.idx];
                let min_x = MIN_VISIBLE - window.w as isize;
                let max_x = self.width as isize - MIN_VISIBLE;
                let max_y = (taskbar_y - TITLEBAR_H as isize).max(0);
                let new_x = (state.x as isize - drag.off_x).clamp(min_x, max_x);
                let new_y = (state.y as isize - drag.off_y).clamp(0, max_y);
                if window.x != new_x || window.y != new_y {
                    window.x = new_x;
                    window.y = new_y;
                    crate::serial_println!("DESKTOP: moved window to ({},{})", new_x, new_y);
                    full_redraw = true;
                }
            }
        }

        if state.left_pressed {
            crate::serial_println!("DESKTOP: press at ({},{})", state.x, state.y);
            full_redraw |= self.press(state.x, state.y);
        }

        let new_hover = if self.drag.is_none() && !state.left_down {
            self.hover_at(state.x, state.y)
        } else {
            Hover::None
        };
        if new_hover != self.hover {
            self.hover = new_hover;
            full_redraw = true;
        }

        full_redraw
    }

    // ------------------------------------------------------------------
    // Pixel primitives
    // ------------------------------------------------------------------

    fn encode_pixel(&self, color: Rgb) -> [u8; 4] {
        match self.format {
            PixelFormat::Bgr => [color.2, color.1, color.0, 0],
            PixelFormat::Rgb => [color.0, color.1, color.2, 0],
            PixelFormat::U8 => {
                let luma = ((u16::from(color.0) * 77
                    + u16::from(color.1) * 150
                    + u16::from(color.2) * 29)
                    >> 8) as u8;
                [luma, 0, 0, 0]
            }
            _ => [color.2, color.1, color.0, 0],
        }
    }

    fn put_pixel(&self, buf: &mut [u8], x: usize, y: usize, color: Rgb) {
        if x >= self.width || y >= self.height || self.bpp == 0 {
            return;
        }
        let index = (y * self.stride + x) * self.bpp;
        let encoded = self.encode_pixel(color);
        if self.bpp == 3 {
            if index + 3 <= buf.len() {
                buf[index] = encoded[0];
                buf[index + 1] = encoded[1];
                buf[index + 2] = encoded[2];
            }
        } else {
            let bytes = self.bpp.min(encoded.len());
            if index + bytes <= buf.len() {
                buf[index..index + bytes].copy_from_slice(&encoded[..bytes]);
            }
        }
    }

    fn fill_rect(
        &self,
        buf: &mut [u8],
        x: isize,
        y: isize,
        w: usize,
        h: usize,
        color: Rgb,
    ) {
        if w == 0 || h == 0 || self.bpp == 0 {
            return;
        }

        let x0 = x.max(0) as usize;
        let y0 = y.max(0) as usize;
        let x1 = x
            .saturating_add(w as isize)
            .clamp(0, self.width as isize) as usize;
        let y1 = y
            .saturating_add(h as isize)
            .clamp(0, self.height as isize) as usize;
        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let encoded = self.encode_pixel(color);
        let bpp = self.bpp;
        let span_bytes = (x1 - x0) * bpp;
        for py in y0..y1 {
            let row_start = (py * self.stride + x0) * bpp;
            let row_end = row_start + span_bytes;
            if row_end > buf.len() {
                continue;
            }
            let row = &mut buf[row_start..row_end];
            if bpp == 3 {
                fill_row_3(row, encoded[0], encoded[1], encoded[2]);
            } else {
                let bytes = bpp.min(encoded.len());
                for pixel in row.chunks_exact_mut(bpp) {
                    pixel[..bytes].copy_from_slice(&encoded[..bytes]);
                }
            }
        }
    }

    fn draw_rect_outline(
        &self,
        buf: &mut [u8],
        x: isize,
        y: isize,
        w: usize,
        h: usize,
        color: Rgb,
    ) {
        if w == 0 || h == 0 {
            return;
        }
        self.fill_rect(buf, x, y, w, 1, color);
        self.fill_rect(buf, x, y + h as isize - 1, w, 1, color);
        self.fill_rect(buf, x, y, 1, h, color);
        self.fill_rect(buf, x + w as isize - 1, y, 1, h, color);
    }

    // ------------------------------------------------------------------
    // Text
    // ------------------------------------------------------------------

    fn draw_char(
        &self,
        buf: &mut [u8],
        x: isize,
        y: isize,
        scale: usize,
        byte: u8,
        color: Rgb,
    ) {
        let rows = font_rows(byte);
        let s = scale as isize;
        for (row_index, &bits) in rows.iter().enumerate() {
            for col in 0..5usize {
                if (bits & (1 << (4 - col))) != 0 {
                    self.fill_rect(
                        buf,
                        x + col as isize * s,
                        y + row_index as isize * s,
                        scale,
                        scale,
                        color,
                    );
                }
            }
        }
    }

    fn draw_text(
        &self,
        buf: &mut [u8],
        x: isize,
        y: isize,
        scale: usize,
        text: &str,
        color: Rgb,
    ) {
        if scale == 0 {
            return;
        }
        let step = (6 * scale) as isize;
        let mut cursor_x = x;
        for &byte in text.as_bytes() {
            self.draw_char(buf, cursor_x, y, scale, byte, color);
            cursor_x += step;
        }
    }

    fn draw_text_centered(
        &self,
        buf: &mut [u8],
        rect: Rect,
        scale: usize,
        text: &str,
        color: Rgb,
    ) {
        let text_w = (text.len() * 6 * scale) as isize;
        let text_h = (7 * scale) as isize;
        let x = rect.x + ((rect.w as isize - text_w) / 2).max(0);
        let y = rect.y + ((rect.h as isize - text_h) / 2).max(0);
        self.draw_text(buf, x, y, scale, text, color);
    }

    // ------------------------------------------------------------------
    // Full desktop rendering
    // ------------------------------------------------------------------

    fn draw_background(&self, buf: &mut [u8]) {
        let height = self.height;
        for y in 0..height {
            let t = if height > 1 {
                (y * 255 / (height - 1)) as i32
            } else {
                0
            };
            let color = Rgb(
                lerp(DESKTOP_TOP.0, DESKTOP_BOTTOM.0, t),
                lerp(DESKTOP_TOP.1, DESKTOP_BOTTOM.1, t),
                lerp(DESKTOP_TOP.2, DESKTOP_BOTTOM.2, t),
            );
            self.fill_rect(buf, 0, y as isize, self.width, 1, color);
        }

        let mark_x = self.width as isize - 108;
        self.draw_text(buf, mark_x, 16, 2, "RUST OS", DESKTOP_MARK);
        self.draw_text(buf, mark_x + 4, 38, 1, "DESKTOP", DESKTOP_MARK);
    }

    fn draw_window(&self, buf: &mut [u8], idx: usize, active: bool) {
        let window = self.windows[idx];
        let title_color = if active { TITLE_ACTIVE } else { TITLE_INACTIVE };

        self.fill_rect(
            buf,
            window.x + 5,
            window.y + 6,
            window.w,
            window.h,
            SHADOW,
        );
        self.fill_rect(
            buf,
            window.x - 1,
            window.y - 1,
            window.w + 2,
            window.h + 2,
            WIN_BORDER,
        );
        self.fill_rect(
            buf,
            window.x,
            window.y + TITLEBAR_H as isize,
            window.w,
            window.h.saturating_sub(TITLEBAR_H),
            WIN_BG,
        );
        self.fill_rect(
            buf,
            window.x,
            window.y,
            window.w,
            TITLEBAR_H,
            title_color,
        );
        self.fill_rect(buf, window.x, window.y, window.w, 1, if active {
            Rgb(96, 156, 214)
        } else {
            Rgb(154, 168, 184)
        });

        self.fill_rect(buf, window.x + 8, window.y + 8, 12, 12, ICON_BG);
        self.fill_rect(buf, window.x + 11, window.y + 11, 6, 6, ICON_FG);
        self.draw_text(
            buf,
            window.x + 26,
            window.y + 7,
            2,
            window.title,
            TEXT_LIGHT,
        );

        let close = self.close_rect(idx);
        let close_color = if self.hover == Hover::Close(idx) {
            CLOSE_HOVER
        } else {
            CLOSE_BG
        };
        self.fill_rect(
            buf,
            close.x,
            close.y,
            close.w,
            close.h,
            close_color,
        );
        self.draw_text_centered(buf, close, 2, "X", TEXT_LIGHT);

        self.fill_rect(
            buf,
            window.x,
            window.y + TITLEBAR_H as isize - 1,
            window.w,
            1,
            WIN_BORDER,
        );

        match window.kind {
            WindowKind::Welcome => {
                self.draw_text(buf, window.x + 24, window.y + 44, 3, "WELCOME", ACCENT);
                self.fill_rect(
                    buf,
                    window.x + 24,
                    window.y + 78,
                    window.w.saturating_sub(48),
                    1,
                    SEPARATOR,
                );
                self.draw_text(
                    buf,
                    window.x + 24,
                    window.y + 94,
                    2,
                    "DRAG THE TITLE BAR TO MOVE",
                    TEXT_DARK,
                );
                self.draw_text(
                    buf,
                    window.x + 24,
                    window.y + 118,
                    2,
                    "CLICK X TO CLOSE A WINDOW",
                    TEXT_DARK,
                );
                self.draw_text(
                    buf,
                    window.x + 24,
                    window.y + 142,
                    2,
                    "MENU OR TASKBAR TO REOPEN",
                    TEXT_DARK,
                );
                self.draw_text(
                    buf,
                    window.x + 24,
                    window.y + 166,
                    2,
                    "THE COUNTER LOGS TO COM1",
                    TEXT_DARK,
                );
            }
            WindowKind::Counter => {
                self.draw_text(
                    buf,
                    window.x + 26,
                    window.y + 46,
                    2,
                    "CLICK COUNT",
                    TEXT_DARK,
                );

                let mut number_buffer = [0u8; 10];
                let number = u32_to_str(self.clicks, &mut number_buffer);
                self.draw_text(buf, window.x + 26, window.y + 66, 4, number, ACCENT);

                let button = self.counter_button_rect(idx);
                let button_color = if self.hover == Hover::Counter {
                    BTN_HOVER
                } else {
                    BTN_FACE
                };
                self.fill_rect(
                    buf,
                    button.x,
                    button.y,
                    button.w,
                    button.h,
                    button_color,
                );
                self.draw_rect_outline(
                    buf,
                    button.x,
                    button.y,
                    button.w,
                    button.h,
                    WIN_BORDER,
                );
                self.draw_text_centered(buf, button, 2, "CLICK ME +1", TEXT_DARK);

                self.draw_text(
                    buf,
                    window.x + 26,
                    window.y + 188,
                    1,
                    "CLICKS ARE LOGGED TO COM1 (0X3F8)",
                    TEXT_MUTED,
                );
            }
        }
    }

    fn draw_menu(&self, buf: &mut [u8]) {
        let menu = self.menu_rect();

        self.fill_rect(buf, menu.x + 4, menu.y + 5, menu.w, menu.h, SHADOW);
        self.fill_rect(
            buf,
            menu.x - 1,
            menu.y - 1,
            menu.w + 2,
            menu.h + 2,
            WIN_BORDER,
        );
        self.fill_rect(buf, menu.x, menu.y, menu.w, menu.h, WIN_BG);
        self.draw_text(buf, menu.x + 10, menu.y + 6, 1, "PROGRAMS", TEXT_MUTED);
        self.fill_rect(
            buf,
            menu.x + 8,
            menu.y + 16,
            menu.w - 16,
            1,
            SEPARATOR,
        );

        for idx in 0..WINDOW_COUNT {
            let item = self.menu_item_rect(idx);
            let color = if self.hover == Hover::StartItem(idx) {
                BTN_HOVER
            } else {
                BTN_FACE
            };
            self.fill_rect(buf, item.x, item.y, item.w, item.h, color);
            self.draw_rect_outline(buf, item.x, item.y, item.w, item.h, WIN_BORDER);
            self.draw_text(
                buf,
                item.x + 14,
                item.y + 9,
                2,
                self.windows[idx].title,
                TEXT_DARK,
            );
        }
    }

    fn draw_taskbar(&self, buf: &mut [u8], active: Option<usize>) {
        let y = self.taskbar_y();
        self.fill_rect(buf, 0, y, self.width, TASKBAR_H, TASKBAR_BG);
        self.fill_rect(buf, 0, y, self.width, 2, TASKBAR_ACCENT);

        let menu_button = self.menu_button_rect();
        let menu_color = if self.menu_open {
            START_ACTIVE
        } else if self.hover == Hover::Menu {
            START_HOVER
        } else {
            START_IDLE
        };
        self.fill_rect(
            buf,
            menu_button.x,
            menu_button.y,
            menu_button.w,
            menu_button.h,
            menu_color,
        );
        self.draw_text_centered(buf, menu_button, 2, "MENU", TEXT_LIGHT);

        for idx in 0..WINDOW_COUNT {
            let button = self.task_button_rect(idx);
            let is_active = self.windows[idx].open && active == Some(idx);
            let color = if is_active {
                TITLE_ACTIVE
            } else if self.hover == Hover::Task(idx) {
                BTN_HOVER
            } else {
                BTN_FACE
            };
            let text_color = if is_active {
                TEXT_LIGHT
            } else if self.windows[idx].open {
                TEXT_DARK
            } else {
                TEXT_MUTED
            };
            self.fill_rect(buf, button.x, button.y, button.w, button.h, color);
            self.draw_text(
                buf,
                button.x + 10,
                button.y + 6,
                2,
                self.windows[idx].title,
                text_color,
            );
        }

        let mark_x = self.width as isize - 100;
        self.draw_text(buf, mark_x, y + 11, 2, "RUST OS", TEXT_MUTED);
    }

    pub fn render_full(&mut self, buf: &mut [u8], mx: usize, my: usize) {
        self.cursor_active = false;
        self.draw_background(buf);

        let active = self.active_window();
        for &idx in self.z_order.iter() {
            if self.windows[idx].open {
                self.draw_window(buf, idx, active == Some(idx));
            }
        }

        if self.menu_open {
            self.draw_menu(buf);
        }

        self.draw_taskbar(buf, active);
        self.draw_cursor(buf, mx, my);
    }

    // ------------------------------------------------------------------
    // Cursor with a save-under buffer
    // ------------------------------------------------------------------

    fn restore_cursor(&mut self, buf: &mut [u8]) {
        if !self.cursor_active {
            return;
        }
        self.cursor_active = false;

        let bytes = self.bpp.min(4);
        for py in 0..self.cursor_saved_h {
            let row = ((self.cursor_y + py) * self.stride + self.cursor_x) * self.bpp;
            for px in 0..self.cursor_saved_w {
                let dst = row + px * self.bpp;
                let src = (py * CURSOR_W + px) * 4;
                if dst + bytes <= buf.len() {
                    buf[dst..dst + bytes].copy_from_slice(&self.cursor_saved[src..src + bytes]);
                }
            }
        }
    }

    pub fn draw_cursor(&mut self, buf: &mut [u8], mx: usize, my: usize) {
        self.restore_cursor(buf);
        if self.width == 0 || self.height == 0 {
            return;
        }

        let mx = mx.min(self.width - 1);
        let my = my.min(self.height - 1);
        let save_w = CURSOR_W.min(self.width - mx);
        let save_h = CURSOR_H.min(self.height - my);
        if save_w == 0 || save_h == 0 {
            return;
        }

        let bytes = self.bpp.min(4);
        for py in 0..save_h {
            let row = ((my + py) * self.stride + mx) * self.bpp;
            for px in 0..save_w {
                let src = row + px * self.bpp;
                let dst = (py * CURSOR_W + px) * 4;
                if src + bytes <= buf.len() {
                    self.cursor_saved[dst..dst + bytes].copy_from_slice(&buf[src..src + bytes]);
                }
            }
        }
        self.cursor_x = mx;
        self.cursor_y = my;
        self.cursor_saved_w = save_w;
        self.cursor_saved_h = save_h;
        self.cursor_active = true;

        for (row, line) in CURSOR_SHAPE.iter().enumerate() {
            for (col, &cell) in line.as_bytes().iter().enumerate() {
                if cell == b'.' && cursor_has_filled_neighbor(row, col) {
                    self.put_pixel(buf, mx + col, my + row, CURSOR_OUTLINE);
                }
            }
        }
        for (row, line) in CURSOR_SHAPE.iter().enumerate() {
            for (col, &cell) in line.as_bytes().iter().enumerate() {
                if cell == b'X' {
                    self.put_pixel(buf, mx + col, my + row, CURSOR_FILL);
                }
            }
        }
    }
}

/// Fill a row of 3-byte pixels. The 24-byte pattern covers eight pixels, so
/// the bulk of the row is written with three 64-bit stores per iteration.
fn fill_row_3(row: &mut [u8], red: u8, green: u8, blue: u8) {
    let mut pattern = [0u8; 24];
    let mut index = 0;
    while index < 24 {
        pattern[index] = red;
        pattern[index + 1] = green;
        pattern[index + 2] = blue;
        index += 3;
    }
    let first = u64::from_ne_bytes(pattern[0..8].try_into().unwrap());
    let second = u64::from_ne_bytes(pattern[8..16].try_into().unwrap());
    let third = u64::from_ne_bytes(pattern[16..24].try_into().unwrap());

    let pointer = row.as_mut_ptr();
    let mut offset = 0;
    let mut remaining = row.len();
    while remaining >= 24 {
        unsafe {
            let pixel = pointer.add(offset);
            core::ptr::write_unaligned(pixel as *mut u64, first);
            core::ptr::write_unaligned(pixel.add(8) as *mut u64, second);
            core::ptr::write_unaligned(pixel.add(16) as *mut u64, third);
        }
        offset += 24;
        remaining -= 24;
    }
    while remaining >= 3 {
        unsafe {
            let pixel = pointer.add(offset);
            *pixel = red;
            *pixel.add(1) = green;
            *pixel.add(2) = blue;
        }
        offset += 3;
        remaining -= 3;
    }
}

fn lerp(start: u8, end: u8, t: i32) -> u8 {
    let value = i32::from(start) + (i32::from(end) - i32::from(start)) * t / 255;
    value.clamp(0, 255) as u8
}

fn u32_to_str(mut value: u32, buffer: &mut [u8; 10]) -> &str {
    let mut index = buffer.len();
    if value == 0 {
        buffer[buffer.len() - 1] = b'0';
        return core::str::from_utf8(&buffer[buffer.len() - 1..]).unwrap_or("0");
    }
    while value > 0 {
        index -= 1;
        buffer[index] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    core::str::from_utf8(&buffer[index..]).unwrap_or("0")
}

fn cursor_has_filled_neighbor(row: usize, col: usize) -> bool {
    let first_row = row.saturating_sub(1);
    let first_col = col.saturating_sub(1);
    for r in first_row..=row + 1 {
        if r >= CURSOR_SHAPE.len() {
            continue;
        }
        let line = CURSOR_SHAPE[r].as_bytes();
        for c in first_col..=col + 1 {
            if c < line.len() && line[c] == b'X' {
                return true;
            }
        }
    }
    false
}

fn font_rows(byte: u8) -> [u8; 7] {
    match byte.to_ascii_uppercase() {
        b'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        b'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        b'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        b'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        b'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        b'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        b'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        b'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        b'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        b'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100],
        b'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        b'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        b'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        b'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        b'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        b'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        b'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        b'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        b'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        b'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        b'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        b'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        b'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        b'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        b'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        b'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        b'0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        b'1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        b'2' => [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111],
        b'3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        b'4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        b'5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        b'6' => [0b01110, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b01110],
        b'7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        b'8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        b'9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110],
        b':' => [0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000],
        b'!' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100],
        b'+' => [0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000],
        b'-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        b'.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00110, 0b00110],
        b'/' => [0b00001, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b10000],
        b'?' => [0b01110, 0b10001, 0b00001, 0b00110, 0b00100, 0b00000, 0b00100],
        b'(' => [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010],
        b')' => [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000],
        _ => [0; 7],
    }
}
