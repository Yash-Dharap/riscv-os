use core::arch::global_asm;
use core::ptr::addr_of_mut;

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum TaskState {
    Empty,
    Ready,
    Running,
    Blocked,
    Exited,
}

// Callee-saved registers only. Caller-saved registers (t*, a*) are already
// spilled by the compiler around the call to switch_context, or by
// trap_vector when the task was preempted.
#[repr(C)]
pub struct TaskContext {
    pub ra: usize,
    pub sp: usize,
    pub s: [usize; 12],
}

const TASK_STACK_SIZE: usize = 16 * 1024;

pub struct TaskControlBlock {
    pub context: TaskContext,
    pub state: TaskState,
    pub entry: Option<fn()>,
    pub stack: [u8; TASK_STACK_SIZE],
}

impl TaskControlBlock {
    pub const fn new() -> Self {
        TaskControlBlock {
            context: TaskContext {
                ra: 0,
                sp: 0,
                s: [0; 12],
            },
            state: TaskState::Empty,
            entry: None,
            stack: [0; TASK_STACK_SIZE],
        }
    }
}

const MAX_TASK_NUM: usize = 4;

pub struct TaskManager {
    pub tasks: [TaskControlBlock; MAX_TASK_NUM],
    pub current_task: usize,
    pub task_num: usize,
}

// `[const { .. }; N]` builds the array without requiring TaskControlBlock: Copy.
pub static mut TASK_MANAGER: TaskManager = TaskManager {
    tasks: [const { TaskControlBlock::new() }; MAX_TASK_NUM],
    current_task: 0,
    task_num: 0,
};

// Raw access to the global. Edition 2024 forbids taking `&mut` of a
// `static mut` directly, so we go through a raw pointer.
fn task_manager() -> &'static mut TaskManager {
    unsafe { &mut *addr_of_mut!(TASK_MANAGER) }
}

// ── Interrupt enable helpers (sstatus.SIE = bit 1) ──────────────────────────

// Clears SIE and returns whether it was previously set.
fn interrupts_off() -> bool {
    let old: usize;
    unsafe { core::arch::asm!("csrrci {}, sstatus, 2", out(reg) old) };
    old & 2 != 0
}

fn interrupts_on() {
    unsafe { core::arch::asm!("csrsi sstatus, 2") };
}

// Registers the code that is currently executing (kmain on the boot stack)
// as task 0. Its context is filled in the first time it is switched away from.
pub fn init() {
    let tm = task_manager();
    tm.tasks[0].state = TaskState::Running;
    tm.current_task = 0;
    tm.task_num = 1;
}

pub fn add_task(program: fn()) {
    let was_on = interrupts_off();
    let tm = task_manager();

    if tm.task_num < MAX_TASK_NUM {
        let index = tm.task_num;
        let task = &mut tm.tasks[index];

        // Stacks grow downwards, and the RISC-V ABI requires sp to be
        // 16-byte aligned, so point sp at the aligned end of the array.
        let stack_top = (task.stack.as_ptr() as usize + TASK_STACK_SIZE) & !0xF;

        // The first switch_context into this task will `ret` to task_start,
        // which enables interrupts and then calls `program`.
        task.context.ra = task_start as *const () as usize;
        task.context.sp = stack_top;
        task.entry = Some(program);
        task.state = TaskState::Ready;

        // Publish the task only after it is fully initialised.
        tm.task_num += 1;
    }

    if was_on {
        interrupts_on();
    }
}

// First code every new task runs. We arrive here from switch_context inside
// schedule(), which runs with interrupts disabled, so turn them back on;
// otherwise the timer could never preempt this task.
extern "C" fn task_start() -> ! {
    let entry = {
        let tm = task_manager();
        tm.tasks[tm.current_task].entry
    };

    interrupts_on();

    if let Some(program) = entry {
        program();
    }

    task_exit();
}

// Called when a task's function returns. The task is never scheduled again.
pub fn task_exit() -> ! {
    {
        let tm = task_manager();
        let current = tm.current_task;
        tm.tasks[current].state = TaskState::Exited;
    }
    loop {
        schedule();
    }
}

global_asm!(
    ".global switch_context",
    "switch_context:",

    // Save current task context (a0 points to old TaskContext)
    "sd ra, 0(a0)",
    "sd sp, 8(a0)",
    "sd s0, 16(a0)",
    "sd s1, 24(a0)",
    "sd s2, 32(a0)",
    "sd s3, 40(a0)",
    "sd s4, 48(a0)",
    "sd s5, 56(a0)",
    "sd s6, 64(a0)",
    "sd s7, 72(a0)",
    "sd s8, 80(a0)",
    "sd s9, 88(a0)",
    "sd s10, 96(a0)",
    "sd s11, 104(a0)",

    // Restore next task context (a1 points to new TaskContext)
    "ld ra, 0(a1)",
    "ld sp, 8(a1)",
    "ld s0, 16(a1)",
    "ld s1, 24(a1)",
    "ld s2, 32(a1)",
    "ld s3, 40(a1)",
    "ld s4, 48(a1)",
    "ld s5, 56(a1)",
    "ld s6, 64(a1)",
    "ld s7, 72(a1)",
    "ld s8, 80(a1)",
    "ld s9, 88(a1)",
    "ld s10, 96(a1)",
    "ld s11, 104(a1)",

    // Jump to the restored 'ra'
    "ret"
);

unsafe extern "C" {
    pub fn switch_context(old: *mut TaskContext, new: *const TaskContext);
}

// Round-robin scheduler. Called from two places:
//   1. yield_task()        - a task voluntarily gives up the CPU
//   2. the timer interrupt - the current task's time slice ran out
// Interrupts are disabled while the task table is being changed so a timer
// interrupt can't re-enter the scheduler halfway through a switch.
pub fn schedule() {
    let was_on = interrupts_off();

    let switch = {
        let tm = task_manager();
        let n = tm.task_num;
        let current_index = tm.current_task;

        // Look at the tasks after the current one, wrapping around,
        // and pick the first one that is Ready.
        let mut next = None;
        for i in 1..n {
            let index = (current_index + i) % n;
            if tm.tasks[index].state == TaskState::Ready {
                next = Some(index);
                break;
            }
        }

        match next {
            None => None, // nobody else is ready, keep running the current task
            Some(next_index) => {
                // 1. Current task goes to the back of the line
                //    (unless it has exited or blocked).
                let current_task = &mut tm.tasks[current_index];
                if current_task.state == TaskState::Running {
                    current_task.state = TaskState::Ready;
                }
                let old_context_ptr = &mut current_task.context as *mut TaskContext;

                // 2. Next task becomes Running.
                let next_task = &mut tm.tasks[next_index];
                next_task.state = TaskState::Running;
                let new_context_ptr = &next_task.context as *const TaskContext;

                // 3. Update the global current task index.
                tm.current_task = next_index;

                Some((old_context_ptr, new_context_ptr))
            }
        }
    };

    if let Some((old, new)) = switch {
        // 4. Perform the context switch. This call "returns" only when some
        //    later schedule() switches back to this task.
        unsafe { switch_context(old, new) };
    }

    // Each task restores its own interrupt state: `was_on` lives on this
    // task's stack, so it is the value from when *this* task entered.
    if was_on {
        interrupts_on();
    }
}

// A helper function a task can call to voluntarily give up the CPU
pub fn yield_task() {
    schedule();
}
