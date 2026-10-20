//! Measures live heap bytes (counting allocator) vs RSS for an index build.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use fossick_bench::{TreeSpec, build_index, rss_bytes};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);

// SAFETY: delegates to the system allocator and only counts bytes.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        // SAFETY: forwarded.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: forwarded.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        LIVE.fetch_add(n, Ordering::Relaxed);
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: forwarded.
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn trim() {
    // SAFETY: malloc_trim has no preconditions.
    #[allow(unsafe_code)]
    unsafe {
        libc::malloc_trim(0);
    }
}

fn main() {
    trim();
    let (r0, l0) = (rss_bytes().0, LIVE.load(Ordering::Relaxed));
    let (w, _) = build_index(&TreeSpec::new(1_000_000));
    trim();
    let (r1, l1) = (rss_bytes().0, LIVE.load(Ordering::Relaxed));
    let g = w.handle().load();
    let st = g.memory_stats();
    println!(
        "rss growth {} MiB, live heap growth {} MiB",
        (r1 - r0) >> 20,
        (l1 - l0) >> 20
    );
    println!(
        "accounted: gen {} MiB + private {} MiB",
        st.total() >> 20,
        w.private_heap_bytes() >> 20
    );
    println!("changeset: {} entries", g.changes().len());
}
