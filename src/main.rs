//! A minimal Windows-like bare-metal desktop for `x86_64-unknown-none`.
//!
//! Boot flow:
//!   * set up COM1 logging,
//!   * take the framebuffer provided by `bootloader_api`,
//!   * install our own IDT, remap the 8259 PIC, start the PIT at 100 Hz,
//!     and initialize the PS/2 auxiliary (mouse) port,
//!   * render the desktop and enter an interrupt-driven event loop.

#![feature(abi_x86_interrupt)]
#![no_std]
#![no_main]

mod desktop;
mod input;
mod serial;

use bootloader_api::config::Mapping;
use bootloader_api::{entry_point, BootInfo, BootloaderConfig};
use core::arch::asm;
use core::mem::size_of;
use core::panic::PanicInfo;

pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

// ---------------------------------------------------------------------------
// Interrupt descriptor table
// ---------------------------------------------------------------------------

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    zero: u32,
}

impl IdtEntry {
    const MISSING: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        type_attr: 0,
        offset_mid: 0,
        offset_high: 0,
        zero: 0,
    };

    fn set_handler(&mut self, address: u64, code_selector: u16) {
        self.offset_low = (address & 0xFFFF) as u16;
        self.selector = code_selector;
        self.ist = 0;
        self.type_attr = 0x8E; // present, DPL 0, 64-bit interrupt gate
        self.offset_mid = ((address >> 16) & 0xFFFF) as u16;
        self.offset_high = ((address >> 32) & 0xFFFF_FFFF) as u32;
        self.zero = 0;
    }
}

#[repr(C, packed)]
struct IdtDescriptor {
    limit: u16,
    base: u64,
}

#[repr(C, align(16))]
struct Idt {
    entries: [IdtEntry; 256],
}

static mut IDT: Idt = Idt {
    entries: [IdtEntry::MISSING; 256],
};

#[repr(C)]
struct InterruptStackFrame {
    instruction_pointer: u64,
    code_segment: u64,
    cpu_flags: u64,
    stack_pointer: u64,
    stack_segment: u64,
}

unsafe fn install_idt() {
    let code_selector: u16;
    unsafe {
        asm!(
            "mov {0:x}, cs",
            out(reg) code_selector,
            options(nomem, nostack, preserves_flags)
        );
    }

    let entries = core::ptr::addr_of_mut!(IDT.entries).cast::<IdtEntry>();
    for vector in 0..256usize {
        let handler = if vector < 32 {
            exception_handler as *const () as u64
        } else {
            ignore_irq as *const () as u64
        };
        unsafe {
            (*entries.add(vector)).set_handler(handler, code_selector);
        }
    }

    unsafe {
        (*entries.add(0x20)).set_handler(timer_irq as *const () as u64, code_selector);
        (*entries.add(0x21)).set_handler(keyboard_irq as *const () as u64, code_selector);
        (*entries.add(0x2C)).set_handler(mouse_irq as *const () as u64, code_selector);

        let descriptor = IdtDescriptor {
            limit: (size_of::<[IdtEntry; 256]>() - 1) as u16,
            base: entries as u64,
        };
        asm!(
            "lidt [{}]",
            in(reg) &descriptor,
            options(readonly, nostack, preserves_flags)
        );
    }
}

// ---------------------------------------------------------------------------
// 8259 PIC, PIT and PS/2 controller
// ---------------------------------------------------------------------------

const PIC1_COMMAND: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_COMMAND: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

unsafe fn io_wait() {
    unsafe {
        serial::outb(0x80, 0);
    }
}

/// Remap the PIC to vectors 0x20..0x2F and mask every interrupt for now.
unsafe fn init_pic() {
    unsafe {
        serial::outb(PIC1_COMMAND, 0x11);
        io_wait();
        serial::outb(PIC2_COMMAND, 0x11);
        io_wait();
        serial::outb(PIC1_DATA, 0x20);
        io_wait();
        serial::outb(PIC2_DATA, 0x28);
        io_wait();
        serial::outb(PIC1_DATA, 0x04);
        io_wait();
        serial::outb(PIC2_DATA, 0x02);
        io_wait();
        serial::outb(PIC1_DATA, 0x01);
        io_wait();
        serial::outb(PIC2_DATA, 0x01);
        io_wait();

        // Mask everything; only IRQ0 (timer), IRQ2 (cascade) and IRQ12
        // (PS/2 mouse) are enabled later.
        serial::outb(PIC1_DATA, 0xFF);
        serial::outb(PIC2_DATA, 0xFF);
    }
}

/// Program PIT channel 0 to roughly 100 Hz.
unsafe fn init_pit() {
    const DIVISOR: u16 = 11_932; // 1.193182 MHz / 11932 ~= 100 Hz
    unsafe {
        serial::outb(0x43, 0x36); // channel 0, lobyte/hibyte, mode 3, binary
        serial::outb(0x40, DIVISOR as u8);
        serial::outb(0x40, (DIVISOR >> 8) as u8);
    }
}

fn ps2_wait_input_empty() -> bool {
    for _ in 0..100_000 {
        let status = unsafe { serial::inb(0x64) };
        if (status & 0x02) == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn ps2_wait_output_full() -> bool {
    for _ in 0..100_000 {
        let status = unsafe { serial::inb(0x64) };
        if (status & 0x01) != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn ps2_flush_output() {
    for _ in 0..64 {
        let status = unsafe { serial::inb(0x64) };
        if (status & 0x01) == 0 {
            return;
        }
        let _ = unsafe { serial::inb(0x60) };
    }
}

fn ps2_write_mouse(command: u8) -> bool {
    if !ps2_wait_input_empty() {
        return false;
    }
    unsafe {
        serial::outb(0x64, 0xD4);
    }
    if !ps2_wait_input_empty() {
        return false;
    }
    unsafe {
        serial::outb(0x60, command);
    }

    // Look for the 0xFA acknowledgement; byte packets may be queued in
    // front of it while the device is still streaming.
    for _ in 0..8 {
        if !ps2_wait_output_full() {
            return false;
        }
        if unsafe { serial::inb(0x60) } == 0xFA {
            return true;
        }
    }
    false
}

unsafe fn init_ps2_mouse() {
    unsafe {
        serial::outb(0x64, 0xAD); // disable keyboard port
        serial::outb(0x64, 0xA7); // disable mouse port
    }
    ps2_flush_output();

    // Enable the auxiliary interrupt in the 8042 configuration byte.
    if ps2_wait_input_empty() {
        unsafe {
            serial::outb(0x64, 0x20);
        }
        if ps2_wait_output_full() {
            let mut config = unsafe { serial::inb(0x60) };
            config |= 0x02; // IRQ12 on mouse data
            config &= !0x20; // mouse clock enabled
            config |= 0x10; // keyboard clock disabled
            if ps2_wait_input_empty() {
                unsafe {
                    serial::outb(0x64, 0x60);
                }
                if ps2_wait_input_empty() {
                    unsafe {
                        serial::outb(0x60, config);
                    }
                }
            }
        }
    }
    ps2_flush_output();

    unsafe {
        serial::outb(0x64, 0xA8); // enable auxiliary port
    }

    let _ = ps2_write_mouse(0xF5); // disable data reporting while configuring
    let _ = ps2_write_mouse(0xF6); // set defaults
    let _ = ps2_write_mouse(0xF3);
    let _ = ps2_write_mouse(100); // 100 samples per second
    let _ = ps2_write_mouse(0xF4); // enable data reporting
    ps2_flush_output();
}

// ---------------------------------------------------------------------------
// Interrupt handlers
// ---------------------------------------------------------------------------

fn eoi_master() {
    unsafe {
        serial::outb(PIC1_COMMAND, 0x20);
    }
}

fn eoi_slave_and_master() {
    unsafe {
        serial::outb(PIC2_COMMAND, 0x20);
        serial::outb(PIC1_COMMAND, 0x20);
    }
}

extern "x86-interrupt" fn exception_handler(frame: InterruptStackFrame) -> ! {
    serial_println!(
        "CPU EXCEPTION: RIP={:#x} CS={:#x} RFLAGS={:#x} RSP={:#x} SS={:#x}",
        frame.instruction_pointer,
        frame.code_segment,
        frame.cpu_flags,
        frame.stack_pointer,
        frame.stack_segment
    );
    halt_forever()
}

extern "x86-interrupt" fn ignore_irq(_frame: InterruptStackFrame) {
    eoi_slave_and_master();
}

extern "x86-interrupt" fn timer_irq(_frame: InterruptStackFrame) {
    TIMER_TICKS.fetch_add(1, Ordering::Relaxed);
    eoi_master();
}

extern "x86-interrupt" fn keyboard_irq(_frame: InterruptStackFrame) {
    // The keyboard is disabled, but drain the shared 8042 buffer just in case.
    let status = unsafe { serial::inb(0x64) };
    if (status & 0x01) != 0 {
        let _ = unsafe { serial::inb(0x60) };
    }
    eoi_master();
}

extern "x86-interrupt" fn mouse_irq(_frame: InterruptStackFrame) {
    // QEMU's vmmouse sends IRQ12 as a "data available" notification without
    // data in the 8042; real PS/2 packets do show up with OBF and AUX set.
    let status = unsafe { serial::inb(0x64) };
    if (status & 0x01) != 0 {
        let byte = unsafe { serial::inb(0x60) };
        serial_println!("IRQ12: status {:#04x} byte {:#04x}", status, byte);
        if (status & 0x20) != 0 {
            input::ps2_feed(byte);
        }
    }
    eoi_slave_and_master();
}

fn halt_forever() -> ! {
    loop {
        unsafe {
            asm!("cli", options(nostack));
            asm!("hlt", options(nostack));
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    unsafe {
        asm!("cli", options(nostack));
    }

    serial::init();
    serial_println!("RUST OS: booting");

    let Some(framebuffer) = boot_info.framebuffer.as_mut() else {
        serial_println!("RUST OS: no framebuffer available");
        halt_forever();
    };

    let info = framebuffer.info();
    let buffer: &'static mut [u8] = framebuffer.buffer_mut();
    serial_println!(
        "RUST OS: framebuffer {}x{}, {} bpp, stride {}",
        info.width,
        info.height,
        info.bytes_per_pixel,
        info.stride
    );

    input::init(info.width, info.height);
    let mut desktop = desktop::Desktop::new(info);

    unsafe {
        install_idt();
        init_pic();
        init_pit();
        init_ps2_mouse();

        // Unmask IRQ0 (timer), IRQ2 (PIC cascade) and IRQ12 (mouse).
        serial::outb(PIC1_DATA, 0xFA);
        serial::outb(PIC2_DATA, 0xEF);
        asm!("sti", options(nostack));
    }
    serial_println!("RUST OS: interrupts enabled, desktop ready");

    let state = input::take_state();
    let start = TIMER_TICKS.load(Ordering::Relaxed);
    desktop.render_full(buffer, state.x, state.y);
    let end = TIMER_TICKS.load(Ordering::Relaxed);
    serial_println!("RENDER: initial full redraw took {} ticks", end - start);

    loop {
        unsafe {
            asm!("hlt", options(nostack));
        }

        input::poll_absolute();
        let state = input::take_state();
        if !state.moved && !state.left_pressed && !state.left_released {
            continue;
        }

        let before = TIMER_TICKS.load(Ordering::Relaxed);
        let full_redraw = desktop.on_mouse(&state);
        let after_logic = TIMER_TICKS.load(Ordering::Relaxed);
        if full_redraw {
            desktop.render_full(buffer, state.x, state.y);
            let after_render = TIMER_TICKS.load(Ordering::Relaxed);
            serial_println!(
                "RENDER: logic {} ticks, full redraw {} ticks",
                after_logic - before,
                after_render - after_logic
            );
        } else if state.moved {
            desktop.draw_cursor(buffer, state.x, state.y);
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("KERNEL PANIC: {}", info);
    halt_forever()
}
