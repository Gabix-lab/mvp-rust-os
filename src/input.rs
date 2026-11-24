//! Mouse input: PS/2 (IRQ12) plus the VMware/QEMU absolute-pointer backdoor.

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU8, Ordering};

// The VMware backdoor ABI needs values in EAX, EBX, ECX and EDX. LLVM reserves
// RBX, so this tiny helper does the register shuffling in assembly and copies
// the three result registers to the caller-provided array.
core::arch::global_asm!(
    ".global mvp_vm_backdoor",
    ".type mvp_vm_backdoor, @function",
    "mvp_vm_backdoor:",
    "push rbx",
    "push r12",
    "mov r12, rdx",
    "mov eax, 0x564d5868",
    "mov ecx, edi",
    "mov ebx, esi",
    "mov edx, 0x5658",
    "in eax, dx",
    "mov [r12], eax",
    "mov [r12 + 4], ebx",
    "mov [r12 + 8], ecx",
    "pop r12",
    "pop rbx",
    "ret",
    ".size mvp_vm_backdoor, . - mvp_vm_backdoor",
);

extern "C" {
    /// `result` receives EAX, EBX and ECX after the backdoor call.
    fn mvp_vm_backdoor(command: u32, argument: u32, result: *mut u32);
}

const VMMOUSE_MAGIC: u32 = 0x564D_5868;

const CMD_GET_VERSION: u32 = 0x0A;
const CMD_ABSPOINTER_STATUS: u32 = 0x28;
const CMD_ABSPOINTER_COMMAND: u32 = 0x29;
const CMD_ABSPOINTER_DATA: u32 = 0x27;

const ARG_READ_ID: u32 = 0x4541_4552;
const ARG_ABSOLUTE: u32 = 0x5342_4152;
const ARG_DISABLE: u32 = 0x0000_00F5;

const VMMOUSE_VERSION: u32 = 0x3442_554A;
const VMMOUSE_ERROR: u32 = 0xFFFF_0000;
const VMMOUSE_RELATIVE: u32 = 0x0001_0000;
const VMMOUSE_LEFT_BUTTON: u32 = 0x20;

static SCREEN_W: AtomicU32 = AtomicU32::new(1280);
static SCREEN_H: AtomicU32 = AtomicU32::new(720);

static CUR_X: AtomicI32 = AtomicI32::new(640);
static CUR_Y: AtomicI32 = AtomicI32::new(360);
static LEFT_DOWN: AtomicBool = AtomicBool::new(false);
static LEFT_PRESSED_EDGE: AtomicBool = AtomicBool::new(false);
static LEFT_RELEASED_EDGE: AtomicBool = AtomicBool::new(false);
static DIRTY: AtomicBool = AtomicBool::new(true);

static PS2_PACKET: [AtomicU8; 3] = [AtomicU8::new(0), AtomicU8::new(0), AtomicU8::new(0)];
static PS2_CYCLE: AtomicU8 = AtomicU8::new(0);

static VMMOUSE_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Perform a VMware backdoor call and return EAX, EBX and ECX.
#[inline]
unsafe fn vm_call(command: u32, argument: u32) -> (u32, u32, u32) {
    let mut result = [0u32; 3];
    unsafe {
        mvp_vm_backdoor(command, argument, result.as_mut_ptr());
    }
    (result[0], result[1], result[2])
}

/// Detect and enable the VMware/QEMU absolute pointer, if it is present.
unsafe fn probe_vmmouse() {
    let (_, magic, _) = unsafe { vm_call(CMD_GET_VERSION, 0) };
    if magic != VMMOUSE_MAGIC {
        crate::serial_println!("INPUT: no VMware backdoor, using PS/2 mouse");
        return;
    }

    // Ask the device to identify itself: this queues VMMOUSE_VERSION.
    let _ = unsafe { vm_call(CMD_ABSPOINTER_COMMAND, ARG_READ_ID) };
    let (status, _, _) = unsafe { vm_call(CMD_ABSPOINTER_STATUS, 0) };
    let queue_len = status & 0xFFFF;
    if queue_len == 0 || (status & 0xFFFF_0000) == VMMOUSE_ERROR {
        crate::serial_println!("INPUT: backdoor present, no absolute pointer; using PS/2");
        return;
    }

    let (version, _, _) = unsafe { vm_call(CMD_ABSPOINTER_DATA, 1) };
    if version != VMMOUSE_VERSION {
        let _ = unsafe { vm_call(CMD_ABSPOINTER_COMMAND, ARG_DISABLE) };
        crate::serial_println!(
            "INPUT: unexpected vmmouse id {:#010x}; using PS/2",
            version
        );
        return;
    }

    let _ = unsafe { vm_call(CMD_ABSPOINTER_COMMAND, ARG_ABSOLUTE) };
    VMMOUSE_AVAILABLE.store(true, Ordering::Release);
    crate::serial_println!("INPUT: VMMOUSE absolute pointer enabled (PS/2 fallback armed)");
}

fn disable_vmmouse(reason: &str) {
    if VMMOUSE_AVAILABLE.swap(false, Ordering::AcqRel) {
        let _ = unsafe { vm_call(CMD_ABSPOINTER_COMMAND, ARG_DISABLE) };
        crate::serial_println!("INPUT: vmmouse error ({}), falling back to PS/2", reason);
    }
}

/// Initialize the mouse subsystem. `width` and `height` are framebuffer pixels.
pub fn init(width: usize, height: usize) {
    SCREEN_W.store(width.max(1) as u32, Ordering::Release);
    SCREEN_H.store(height.max(1) as u32, Ordering::Release);
    CUR_X.store((width / 2) as i32, Ordering::Release);
    CUR_Y.store((height / 2) as i32, Ordering::Release);
    DIRTY.store(true, Ordering::Release);

    unsafe {
        probe_vmmouse();
    }
}

fn set_pos(x: i32, y: i32) {
    let width = SCREEN_W.load(Ordering::Acquire) as i32;
    let height = SCREEN_H.load(Ordering::Acquire) as i32;
    let new_x = x.clamp(0, (width - 1).max(0));
    let new_y = y.clamp(0, (height - 1).max(0));
    let old_x = CUR_X.swap(new_x, Ordering::AcqRel);
    let old_y = CUR_Y.swap(new_y, Ordering::AcqRel);
    if old_x != new_x || old_y != new_y {
        DIRTY.store(true, Ordering::Release);
    }
}

fn set_left_button(down: bool) {
    let was_down = LEFT_DOWN.swap(down, Ordering::AcqRel);
    if down && !was_down {
        LEFT_PRESSED_EDGE.store(true, Ordering::Release);
        DIRTY.store(true, Ordering::Release);
    } else if !down && was_down {
        LEFT_RELEASED_EDGE.store(true, Ordering::Release);
        DIRTY.store(true, Ordering::Release);
    }
}

/// Feed one byte from the PS/2 auxiliary port into the 3-byte packet parser.
///
/// This is called from the IRQ12 handler and must stay short and non-blocking.
pub fn ps2_feed(byte: u8) {
    // QEMU/VMware emit a fake PS/2 packet (dx=1, buttons=0) for every
    // absolute-pointer event; those bytes are only an IRQ12 wake-up, not real
    // input. While the absolute pointer is active, the PS/2 stream is ignored;
    // if it ever fails, `VMMOUSE_AVAILABLE` is cleared and PS/2 takes over.
    if VMMOUSE_AVAILABLE.load(Ordering::Acquire) {
        return;
    }

    match PS2_CYCLE.load(Ordering::Acquire) {
        0 => {
            // Bit 3 is always set in the first byte of a PS/2 mouse packet;
            // anything else is a stale byte and is ignored.
            if (byte & 0x08) != 0 {
                PS2_PACKET[0].store(byte, Ordering::Relaxed);
                PS2_CYCLE.store(1, Ordering::Release);
            }
        }
        1 => {
            PS2_PACKET[1].store(byte, Ordering::Relaxed);
            PS2_CYCLE.store(2, Ordering::Release);
        }
        _ => {
            PS2_PACKET[2].store(byte, Ordering::Relaxed);
            PS2_CYCLE.store(0, Ordering::Release);

            let flags = PS2_PACKET[0].load(Ordering::Relaxed);
            let dx = PS2_PACKET[1].load(Ordering::Relaxed) as i8 as i32;
            let dy = PS2_PACKET[2].load(Ordering::Relaxed) as i8 as i32;

            let x = CUR_X.load(Ordering::Acquire);
            let y = CUR_Y.load(Ordering::Acquire);
            if (flags & 0xC0) == 0 {
                set_pos(x + dx * 2, y - dy * 2);
            }
            set_left_button((flags & 0x01) != 0);
        }
    }
}

/// Drain the absolute pointer queue. Called from the main loop, not from IRQ.
pub fn poll_absolute() {
    if !VMMOUSE_AVAILABLE.load(Ordering::Acquire) {
        return;
    }

    for _ in 0..64 {
        let (status, _, _) = unsafe { vm_call(CMD_ABSPOINTER_STATUS, 0) };
        if (status & 0xFFFF_0000) == VMMOUSE_ERROR {
            disable_vmmouse("status error");
            return;
        }
        if (status & 0xFFFF) < 4 {
            return;
        }

        let (flags, x, y) = unsafe { vm_call(CMD_ABSPOINTER_DATA, 4) };
        if (flags & 0xFFFF_0000) == VMMOUSE_ERROR {
            disable_vmmouse("data error");
            return;
        }

        if (flags & VMMOUSE_RELATIVE) != 0 {
            let cursor_x = CUR_X.load(Ordering::Acquire);
            let cursor_y = CUR_Y.load(Ordering::Acquire);
            set_pos(cursor_x + (x as i32), cursor_y - (y as i32));
        } else {
            let width = SCREEN_W.load(Ordering::Acquire).saturating_sub(1).max(1);
            let height = SCREEN_H.load(Ordering::Acquire).saturating_sub(1).max(1);
            let px = ((x.min(0xFFFF) as u64 * u64::from(width)) / 0xFFFF) as i32;
            let py = ((y.min(0xFFFF) as u64 * u64::from(height)) / 0xFFFF) as i32;
            set_pos(px, py);
        }

        set_left_button((flags & VMMOUSE_LEFT_BUTTON) != 0);
    }
}

/// Snapshot of the mouse state, with click edges consumed by the caller.
#[derive(Clone, Copy)]
pub struct MouseState {
    pub x: usize,
    pub y: usize,
    pub left_down: bool,
    pub left_pressed: bool,
    pub left_released: bool,
    /// True when the position or a button changed since the previous snapshot.
    pub moved: bool,
}

/// Read the current mouse state and atomically clear the one-shot edges.
pub fn take_state() -> MouseState {
    MouseState {
        x: CUR_X.load(Ordering::Acquire).max(0) as usize,
        y: CUR_Y.load(Ordering::Acquire).max(0) as usize,
        left_down: LEFT_DOWN.load(Ordering::Acquire),
        left_pressed: LEFT_PRESSED_EDGE.swap(false, Ordering::AcqRel),
        left_released: LEFT_RELEASED_EDGE.swap(false, Ordering::AcqRel),
        moved: DIRTY.swap(false, Ordering::AcqRel),
    }
}
