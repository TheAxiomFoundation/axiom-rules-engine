//! A global allocator for the test binaries that pin dense execution's
//! fallible allocation. Include it with `#[path]` and declare
//! `#[global_allocator] static ALLOCATOR: Guarded = Guarded;` in the binary.
//!
//! It refuses three kinds of request:
//!
//! - **Above a cap.** [`capped`] refuses every single allocation larger than
//!   a cap. A relation's 8-byte owners can fit under it while a wider buffer
//!   for the same rows does not, and a text longer than the cap cannot be
//!   copied at all, so both cases are reachable with small, real batches.
//! - **A numbered large one.** [`refusing_large`] numbers the allocations of
//!   at least a threshold, from 1, and refuses the one it is given. Running a
//!   case once to count them and then once per number refuses each large
//!   allocation in turn, not only the first.
//! - **Past a live ceiling.** Anything that would take the binary's live bytes
//!   past 1 GiB is refused. A regression that walks the rows allocating as it
//!   goes, such as recording an error for each of 2^50 rows, then aborts
//!   promptly instead of exhausting the host.
//!
//! A cap and a numbered refusal apply only on the thread that set them, so a
//! test's refusals never reach the harness's own threads. Every test takes
//! [`serial`], so one test's refusals never meet another's allocations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Guarded;

/// The largest single allocation granted; `usize::MAX` grants everything.
static CAP: AtomicUsize = AtomicUsize::new(usize::MAX);
/// Allocations of at least this many bytes are numbered.
static LARGE: AtomicUsize = AtomicUsize::new(usize::MAX);
/// The large allocations seen since numbering began.
static LARGE_SEEN: AtomicUsize = AtomicUsize::new(0);
/// The number of the large allocation to refuse; 0 refuses none.
static REFUSED: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// More live bytes than any case here needs by two orders of magnitude.
const LIVE_CEILING: usize = 1 << 30;
static SERIAL: Mutex<()> = Mutex::new(());

thread_local! {
    /// Whether the cap and the numbering apply on this thread. Initialized
    /// in place with no destructor, so reading it never allocates.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

impl Guarded {
    fn armed() -> bool {
        ARMED.try_with(Cell::get).unwrap_or(false)
    }

    /// Whether to refuse a request for `size` bytes: above the cap, or the
    /// numbered large allocation.
    fn refuses(size: usize) -> bool {
        if !Self::armed() {
            return false;
        }
        if size > CAP.load(Ordering::SeqCst) {
            return true;
        }
        if size < LARGE.load(Ordering::SeqCst) {
            return false;
        }
        let number = LARGE_SEEN.fetch_add(1, Ordering::SeqCst) + 1;
        number == REFUSED.load(Ordering::SeqCst)
    }

    /// Count `size` more live bytes, unless that passes the ceiling.
    fn admit(size: usize) -> bool {
        let before = LIVE.fetch_add(size, Ordering::SeqCst);
        if before.saturating_add(size) > LIVE_CEILING {
            LIVE.fetch_sub(size, Ordering::SeqCst);
            return false;
        }
        true
    }
}

unsafe impl GlobalAlloc for Guarded {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if Self::refuses(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded unchanged to the system allocator, which decides
        // first, so a request no host can grant is the host's refusal.
        let pointer = unsafe { System.alloc(layout) };
        if pointer.is_null() || Self::admit(layout.size()) {
            return pointer;
        }
        // SAFETY: `pointer` was just allocated by `System` with `layout`.
        unsafe { System.dealloc(pointer, layout) };
        std::ptr::null_mut()
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if Self::refuses(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: as in `alloc`.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if pointer.is_null() || Self::admit(layout.size()) {
            return pointer;
        }
        // SAFETY: `pointer` was just allocated by `System` with `layout`.
        unsafe { System.dealloc(pointer, layout) };
        std::ptr::null_mut()
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old_size = layout.size();
        // A shrink asks for nothing new.
        if new_size > old_size && Self::refuses(new_size) {
            return std::ptr::null_mut();
        }
        if new_size > old_size && !Self::admit(new_size - old_size) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded unchanged to the system allocator.
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if new_size > old_size {
            if moved.is_null() {
                LIVE.fetch_sub(new_size - old_size, Ordering::SeqCst);
            }
        } else if !moved.is_null() {
            LIVE.fetch_sub(old_size - new_size, Ordering::SeqCst);
        }
        moved
    }
}

pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Disarms the thread and lifts every refusal when dropped, so a failing
/// assertion never leaves one set.
struct Disarm;

impl Drop for Disarm {
    fn drop(&mut self) {
        ARMED.with(|armed| armed.set(false));
        CAP.store(usize::MAX, Ordering::SeqCst);
        LARGE.store(usize::MAX, Ordering::SeqCst);
        REFUSED.store(0, Ordering::SeqCst);
        LARGE_SEEN.store(0, Ordering::SeqCst);
    }
}

/// Run `run` with every single allocation above `cap` bytes refused.
pub fn capped<T>(cap: usize, run: impl FnOnce() -> T) -> T {
    let _disarm = Disarm;
    CAP.store(cap, Ordering::SeqCst);
    ARMED.with(|armed| armed.set(true));
    run()
}

/// Run `run`, numbering the allocations of at least `threshold` bytes from 1
/// and refusing the one numbered `refused` (0 refuses none). Returns `run`'s
/// result and how many large allocations it asked for, the refused one
/// included.
pub fn refusing_large<T>(threshold: usize, refused: usize, run: impl FnOnce() -> T) -> (T, usize) {
    let _disarm = Disarm;
    LARGE_SEEN.store(0, Ordering::SeqCst);
    REFUSED.store(refused, Ordering::SeqCst);
    LARGE.store(threshold, Ordering::SeqCst);
    ARMED.with(|armed| armed.set(true));
    let result = run();
    // Read before `_disarm` resets the count.
    let seen = LARGE_SEEN.load(Ordering::SeqCst);
    (result, seen)
}
