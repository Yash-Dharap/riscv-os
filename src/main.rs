#![no_std]
#![no_main]

extern crate alloc;

mod paging;
mod pmm;
mod trap;
mod uart;
mod process;

use core::arch::global_asm;
use core::panic::PanicInfo;
use spin::MutexGuard;

use core::sync::atomic::{AtomicUsize, Ordering};
use paging::{PageTable, PageTableEntryFlags};

use crate::pmm::PMM;
use crate::{paging::KERNEL_PAGE_TABLE, pmm::PhysicalMemoryManager};

pub static TICK_COUNT: AtomicUsize = AtomicUsize::new(0);
pub const UART_PADDR: usize = 0x1000_0000;
pub static mut EARLY_PMM: Option<PhysicalMemoryManager> = None;

global_asm!(include_str!("entry.s"));

unsafe extern "C" {
    unsafe static _heap_start: usize;
    unsafe static _start: usize;
    unsafe fn trap_vector();
    unsafe static _bss_end: usize;
}

// Busy-wait so each task prints only a few lines per time slice.
fn delay() {
    for _ in 0..500000 { unsafe { core::arch::asm!("nop"); } }
}

// None of these tasks ever call yield_task(): the timer interrupt preempts
// them when their time slice runs out.
fn task_a() {
    let mut count: usize = 0;
    loop {
        crate::uart::uart_print("Task A is running! iteration ");
        crate::uart::uart_print_hex(count);
        crate::uart::uart_print("
");
        count += 1;
        delay();
    }
}

fn task_b() {
    let mut count: usize = 0;
    loop {
        crate::uart::uart_print("Task B is running! iteration ");
        crate::uart::uart_print_hex(count);
        crate::uart::uart_print("
");
        count += 1;
        delay();
    }
}

fn task_c() {
    let mut count: usize = 0;
    loop {
        crate::uart::uart_print("Task C is running! iteration ");
        crate::uart::uart_print_hex(count);
        crate::uart::uart_print("
");
        count += 1;
        delay();
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn kinit() -> usize {
    const KERNEL_OFFSET: usize = 0xFFFFFFFF00000000;
    const RAM_END: usize = 0x88000000;

    let to_phys = |addr: usize| -> usize {
        if addr >= KERNEL_OFFSET {
            addr - KERNEL_OFFSET
        } else {
            addr
        }
    };

    let print_phys = |s: &str| {
        let phys_ptr: *const u8 = to_phys(s.as_ptr() as usize) as *const u8;
        for i in 0..s.len() {
            unsafe { uart::uart_putc_phys(*phys_ptr.add(i)); }
        }
    };

    print_phys("Booting Kernel...\n");

    let heap_start_phys: usize = to_phys(unsafe { core::ptr::addr_of!(_heap_start) as usize });
    
    let mut local_pmm: PhysicalMemoryManager = PhysicalMemoryManager::new(heap_start_phys, RAM_END);

    let root_ptr: *mut PageTable = match local_pmm.alloc_page() {
        Some(ptr) => ptr as *mut PageTable,
        None => loop {}
    };

    let root_table: &'static mut PageTable = unsafe { &mut *root_ptr };
    for i in 0..512 {
        unsafe {
            core::ptr::write_volatile(
                &mut root_table.entries[i],
                paging::PageTableEntry { entry: 0 }
            );
        }
    }

    {
        root_table.map(&mut local_pmm, UART_PADDR, UART_PADDR, PageTableEntryFlags::RWX);
        root_table.map(
            &mut local_pmm, 
            UART_PADDR + KERNEL_OFFSET, 
            UART_PADDR, 
            PageTableEntryFlags::RWX
        );

        print_phys("Building Page Tables..\n");

        let kernel_start_phys: usize = to_phys(core::ptr::addr_of!(_start) as usize);
        let bss_end_phys: usize = to_phys(core::ptr::addr_of!(_bss_end) as usize);

        let mut addr: usize = kernel_start_phys;

        while addr <= bss_end_phys {
            root_table.map(&mut local_pmm, addr, addr, PageTableEntryFlags::RWX);
            addr += 4096;
        }

        let mut phys_addr = 0x8000_0000;
        while phys_addr <= RAM_END {
            root_table.map(&mut local_pmm, phys_addr + KERNEL_OFFSET, phys_addr, PageTableEntryFlags::RWX);
            root_table.map(&mut local_pmm, phys_addr, phys_addr, PageTableEntryFlags::RWX);
            phys_addr += 4096;
        }
    }

    unsafe {
        let early_pmm_phys: *mut Option<PhysicalMemoryManager> = to_phys(core::ptr::addr_of!(EARLY_PMM) as usize) as *mut Option<PhysicalMemoryManager>;
        core::ptr::write_volatile(early_pmm_phys, Some(local_pmm));
    }
    
    print_phys("Page Tables Built! Enabling MMU...\n");
    
    root_ptr as usize
}

#[unsafe(no_mangle)]
pub extern "C" fn kmain(root_ptr_addr: usize) -> ! {
    unsafe {
        let early_ptr: *mut Option<PhysicalMemoryManager> = core::ptr::addr_of_mut!(EARLY_PMM);
        if let Some(early) = core::ptr::replace(early_ptr, None) {
            *PMM.lock() = early;
        }
    }

    let root_table: &'static mut PageTable = unsafe { &mut *(root_ptr_addr as *mut PageTable) };
    *KERNEL_PAGE_TABLE.lock() = Some(root_table);

    pmm::init_vma_pool();

    uart::uart_print("Starting Dynamic memory allocation test...\n");
    uart::uart_print("Initializing a Vec<usize>...\n");

    let mut test_vec: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
    uart::uart_print("Vector initialized!\n");
    uart::uart_print("Inserting Elements...\n");

    for i in 0..100 {
        uart::uart_print("Enterd the loop!\n");
        test_vec.push(i);
        uart::uart_print("Pushed: ");
        uart::uart_print_hex(i);
        uart::uart_print(", ");
    }
    uart::uart_print("\nHeap test: Vector pushed 100 elements successfully!\n");

    uart::uart_print("Enabling Trap Handling...\n");

    let trap_addr: usize = trap_vector as *const () as usize;

    assert!(trap_addr % 4 == 0, "Trap handler must be 4-byte aligned!");

    // Register kmain as task 0 and create the worker tasks BEFORE interrupts
    // are enabled, so the first timer tick sees a complete task table.
    uart::uart_print("Setting up Tasks...
");

    crate::process::init();
    crate::process::add_task(task_a);
    crate::process::add_task(task_b);
    crate::process::add_task(task_c);

    unsafe {
        // First time slice starts now.
        trap::set_next_timer();
        core::arch::asm!("csrw stvec, {}", in(reg) trap_addr);

        uart::uart_print("stvec initalised!
");

        core::arch::asm!(
            "csrrs x0, sie, {}",
            in(reg) 32usize,
            options(nostack, nomem)
        );

        uart::uart_print("STIE initialed
");

        uart::uart_print("Starting Scheduler...
");

        // Global interrupt enable last: from here on the timer preempts tasks.
        core::arch::asm!("csrrsi x0, sstatus, 2", options(nostack, nomem));

        uart::uart_print("sstatus initalised!
");
    }

    uart::uart_print("Trap Handling Enabled!
");

    // kmain is now task 0 and acts as the idle task: it reports ticks and
    // immediately gives its turn to the next task.
    let mut last_tick: usize = 0;
    loop {
        let current_tick: usize = TICK_COUNT.load(Ordering::Relaxed);
        if current_tick != last_tick {
            uart::uart_print("[idle] Tick: ");
            uart::uart_print_hex(current_tick);
            uart::uart_print("
");
            last_tick = current_tick;
        }

        crate::process::yield_task();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    let to_phys = |addr: usize| -> usize {
        if addr >= 0xFFFFFFFF00000000 {
            addr - 0xFFFFFFFF00000000
        } else {
            addr
        }
    };

    let print_phys = |s: &str| {
        let phys_ptr: *const u8 = to_phys(s.as_ptr() as usize) as *const u8;
        for i in 0..s.len() {
            unsafe { uart::uart_putc_phys(*phys_ptr.add(i)); }
        }
    };

    print_phys("\n=== KERNEL PANIC ===\n");
    loop {}
}