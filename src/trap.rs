use crate::TICK_COUNT;
use crate::uart;

use core::sync::atomic::Ordering;

// Length of one round-robin time slice. QEMU virt's timer runs at 10 MHz,
// so 1_000_000 ticks = 100 ms.
pub const TIME_SLICE: usize = 1_000_000;

#[repr(C)]
pub struct TrapContext {
    pub ra: usize,         // offset 0
    pub sp: usize,         // offset 8 (The original SP we saved)
    pub gp: usize,         // offset 16
    pub tp: usize,         // offset 24
    pub regs: [usize; 27], // t0-t6, a0-a7, s0-s11 (offsets 32-240)
    pub sepc: usize,       // offset 248 (restored into sepc before sret)
    pub sstatus: usize,    // offset 256 (restored into sstatus before sret)
}

// Program the next timer interrupt one time slice from now.
pub fn set_next_timer() {
    let now: usize;
    unsafe {
        core::arch::asm!("rdtime {}", out(reg) now); //rdtime = pseudo-instruction to read current timer (mtime)
        core::arch::asm!("csrw stimecmp, {}", in(reg) now + TIME_SLICE);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn trap_handler(ctx: &mut TrapContext) -> () {
    let scause_value: usize;
    let stval_value: usize;

    unsafe {
        core::arch::asm!("csrr {}, scause", out(reg) scause_value);
        core::arch::asm!("csrr {}, stval",  out(reg) stval_value);
    }

    let is_interrupt: bool = (scause_value >> 63) & 1 == 1;
    let cause_code: usize = scause_value & 0xfff; //cause code is stored in the low 12 bits

    if is_interrupt {
        match cause_code {
            5 => {
                // Supervisor timer interrupt: the current task's slice is over.
                TICK_COUNT.fetch_add(1, Ordering::Relaxed);

                // Re-arm the timer BEFORE switching. Until stimecmp moves
                // forward the interrupt stays pending, and the next task
                // would trap again the moment it enabled interrupts.
                set_next_timer();

                // Preempt: put the current task at the back of the queue and
                // run the next Ready one. Returns when this task is picked again.
                crate::process::schedule();
            }

            _ => {}
        }
    } else {
        //Handle Exceptions (page faults, ecall, etc.)
        print_trap_diagnostic(ctx, scause_value, stval_value);

        match cause_code {
            /*
            8: ecall from u-mode
            9: ecall from s-mode
            11: ecall from m-mode
            */
            9 => {
                ctx.sepc += 4;
            }
            3 => {
                uart::uart_print("BREAKPOINT HIT - trap handler works!\n");
                uart::uart_print("Advancing SEPC...\n");
                advance_sepc(ctx);

                uart::uart_print("SEPC advancded to: ");
                uart::uart_print_hex(ctx.sepc);
                uart::uart_print("\n");
            }
            _ => loop {},
        }
    }
}

fn print_trap_diagnostic(ctx: &TrapContext, scause_value: usize, stval_value: usize) {
    uart::uart_print("\n--- TRAP DIAGNOSTIC ---\n");
    uart::uart_print("SCAUSE: ");
    uart::uart_print_hex(scause_value);

    uart::uart_print("\nSEPC  : ");
    uart::uart_print_hex(ctx.sepc);

    uart::uart_print("\nSTVAL : ");
    uart::uart_print_hex(stval_value);

    uart::uart_print("\nRA    : ");
    uart::uart_print_hex(ctx.ra);

    uart::uart_print("\nSP    : ");
    uart::uart_print_hex(ctx.sp);
    uart::uart_print("\n-----------------------\n");
}

// Skip the trapping instruction (2 bytes if compressed, otherwise 4).
// Works on the saved sepc, which trap_vector writes back before sret.
pub fn advance_sepc(ctx: &mut TrapContext) {
    let instruction: u16 = unsafe { core::ptr::read_volatile(ctx.sepc as *const u16) };
    let step: usize = if (instruction & 0b11) != 0b11 { 2 } else { 4 };
    ctx.sepc += step;
}
