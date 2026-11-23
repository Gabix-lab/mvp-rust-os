//! Minimal COM1 (0x3F8) serial output used for kernel logging.

use core::arch::asm;
use core::fmt::{self, Write};

const COM1: u16 = 0x3F8;

/// Write one byte to an x86 I/O port.
///
/// # Safety
/// The caller must ensure that writing to `port` is safe on the current
/// machine (e.g. it is a known UART or PS/2 controller port).
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// Read one byte from an x86 I/O port.
///
/// # Safety
/// The caller must ensure that reading from `port` is safe on the current
/// machine (e.g. it is a known UART or PS/2 controller port).
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!(
            "in al, dx",
            out("al") value,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

/// Initialize COM1 as an 115200 baud, 8N1 serial port with FIFOs enabled.
pub fn init() {
    unsafe {
        outb(COM1 + 1, 0x00); // disable UART interrupts
        outb(COM1 + 3, 0x80); // enable DLAB
        outb(COM1 + 0, 0x01); // divisor low byte: 1 => 115200 baud
        outb(COM1 + 1, 0x00); // divisor high byte
        outb(COM1 + 3, 0x03); // 8 data bits, no parity, one stop bit
        outb(COM1 + 2, 0xC7); // enable and clear FIFOs, 14-byte threshold
        outb(COM1 + 4, 0x0B); // IRQs enabled, RTS/DSR set
    }
}

/// Send a single byte, waiting until the transmit holding register is empty.
pub fn write_byte(byte: u8) {
    unsafe {
        while (inb(COM1 + 5) & 0x20) == 0 {
            core::hint::spin_loop();
        }
        outb(COM1, byte);
    }
}

/// Send a string, translating `\n` into `\r\n`.
pub fn write_str(text: &str) {
    for &byte in text.as_bytes() {
        if byte == b'\n' {
            write_byte(b'\r');
        }
        write_byte(byte);
    }
}

struct SerialPort;

impl Write for SerialPort {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        write_str(text);
        Ok(())
    }
}

/// Implementation detail for [`serial_println!`].
pub fn log_fmt(args: fmt::Arguments<'_>) {
    let _ = SerialPort.write_fmt(args);
}

#[macro_export]
macro_rules! serial_println {
    ($($arg:tt)*) => {{
        $crate::serial::log_fmt(format_args!($($arg)*));
        $crate::serial::write_str("\n");
    }};
}
