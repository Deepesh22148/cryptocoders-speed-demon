use std::cmp::{max, min};
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering::Relaxed};
use std::thread;

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
}

#[derive(Clone, Copy)]
struct Ent { k: u64, v: u32 }

// Raw pointer shared across threads; callers guarantee disjoint writes.
struct P<T>(*mut T);
impl<T> Clone for P<T> { fn clone(&self) -> Self { *self } }
impl<T> Copy for P<T> {}
unsafe impl<T> Send for P<T> {}
unsafe impl<T> Sync for P<T> {}
impl<T> P<T> {
    #[inline(always)] fn at(self, i: usize) -> *mut T { unsafe { self.0.add(i) } }
    #[inline(always)] unsafe fn set(self, i: usize, v: T) { std::ptr::write(self.0.add(i), v) }
}

#[inline(always)]
fn pf<T>(p: *const T) {
    #[cfg(target_arch = "x86_64")]
    unsafe { core::arch::x86_64::_mm_prefetch::<{ core::arch::x86_64::_MM_HINT_T0 }>(p as *const i8); }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = p;
}

fn par(t: usize, f: impl Fn(usize) + Sync) {
    thread::scope(|s| {
        for k in 0..t { let f = &f; s.spawn(move || f(k)); }
    });
}
fn dynp(nt: usize, n: usize, f: impl Fn(usize) + Sync) {
    let c = AtomicUsize::new(0);
    par(nt, |_| loop {
        let k = c.fetch_add(1, Relaxed);
        if k >= n { break; }
        f(k);
    });
}

fn atomic_view(v: &mut [u32]) -> &[AtomicU32] {
    unsafe { &*(v as *mut [u32] as *const [AtomicU32]) }
}

// Drop the pages fully inside edge range [lo, hi) of the u64 array at `base`.
fn release(base: usize, lo: usize, hi: usize) {
    let a = (base + lo * 8 + 4095) & !4095;
    let b = (base + hi * 8) & !4095;
    if b > a { unsafe { madvise(a as *mut u8, b - a, 4); } } // 4 = MADV_DONTNEED
}

// Reads the next unsigned number; leaves p on the byte after it.
fn rd(b: &[u8], p: &mut usize) -> u64 {
    let n = b.len();
    let mut i = *p;
    while i < n && !b[i].is_ascii_digit() { i += 1; }
    let mut x = 0u64;
    while i < n && b[i].is_ascii_digit() { x = x * 10 + (b[i] - b'0') as u64; i += 1; }
    *p = i;
    x
}

// Lines "u v w\n" in [s, e); s and e are line starts.
fn scan(b: &[u8], s: usize, e: usize, mut f: impl FnMut(u32, u32, u32)) {
    let mut p = s;
    while p < e {
        let u = rd(b, &mut p);
        let v = rd(b, &mut p);
        let w = rd(b, &mut p);
        p += 1; // the '\n'
        f(u as u32, v as u32, w as u32);
    }
}

// 4-ary min-heap, lazy deletion
struct Heap { a: Vec<Ent> }
impl Heap {
    fn push(&mut self, e: Ent) {
        let mut i = self.a.len();
        self.a.push(e);
        while i > 0 {
            let p = (i - 1) >> 2;
            if self.a[p].k <= e.k { break; }
            self.a[i] = self.a[p];
            i = p;
        }
        self.a[i] = e;
    }
    fn pop(&mut self) {
        let e = self.a.pop().unwrap();
        let n = self.a.len();
        if n == 0 { return; }
        let mut i = 0;
        loop {
            let c = 4 * i + 1;
            if c >= n { break; }
            let mut b = c;
            for j in c + 1..min(c + 4, n) { if self.a[j].k < self.a[b].k { b = j; } }
            if self.a[b].k >= e.k { break; }
            self.a[i] = self.a[b];
            i = b;
        }
        self.a[i] = e;
    }
}

// label = dist+1, 0 = unlabeled; t = touched nodes, reset after each query
struct Sc { l: Vec<u64>, t: Vec<u32>, h: Heap }

fn solve(off: &[u32], adj: &[u64], s: &mut Sc, src: u32, dst: u32) -> i64 {
    let l = s.l.as_mut_ptr();
    s.h.a.clear();
    s.t.clear();
    unsafe { *l.add(src as usize) = 1; }
    s.t.push(src);
    s.h.push(Ent { k: 0, v: src });
    let mut res = -1i64;
    while let Some(&e) = s.h.a.first() {
        s.h.pop();
        let v = e.v as usize;
        unsafe {
            if *l.add(v) != e.k + 1 { continue; } // stale entry
            if e.v == dst { res = e.k as i64; break; }
            let i0 = *off.get_unchecked(v) as usize;
            let i1 = *off.get_unchecked(v + 1) as usize;
            for i in i0..i1 { pf(l.add(*adj.get_unchecked(i) as u32 as usize)); }
            for i in i0..i1 {
                let x = *adj.get_unchecked(i);
                let to = x as u32 as usize;
                let nd = e.k + (x >> 32);
                let cur = *l.add(to);
                if cur == 0 || nd + 1 < cur {
                    if cur == 0 { s.t.push(to as u32); }
                    *l.add(to) = nd + 1;
                    s.h.push(Ent { k: nd, v: to as u32 });
                }
            }
        }
    }
    for &v in &s.t { unsafe { *l.add(v as usize) = 0; } }
    s.t.clear();
    res
}

fn claim(vis: &[AtomicU8], v: usize) -> bool {
    vis[v].load(Relaxed) == 0 && vis[v].swap(1, Relaxed) == 0
}

// BFS over out-edges from the nodes already in q, appending newly claimed nodes.
fn grow(q: &mut Vec<u32>, off: &[u32], adj: &[u64], vis: &[AtomicU8]) {
    let mut h = 0;
    while h < q.len() {
        let u = q[h] as usize;
        if h + 16 < q.len() { pf(off.as_ptr().wrapping_add(q[h + 16] as usize)); }
        if h + 8 < q.len() { pf(adj.as_ptr().wrapping_add(off[q[h + 8] as usize] as usize)); }
        for i in off[u] as usize..off[u + 1] as usize {
            let v = adj[i] as u32;
            if claim(vis, v as usize) { q.push(v); }
        }
        h += 1;
    }
}

fn main() {
    let nt = thread::available_parallelism().map(|x| x.get()).unwrap_or(12).min(12);

    // ---- map the input ----
    let f = File::open("/data/graph.txt").unwrap();
    let fz = f.metadata().unwrap().len() as usize;
    let b: &'static [u8] = unsafe {
        let p = mmap(std::ptr::null_mut(), fz, 1 /*PROT_READ*/, 2 /*MAP_PRIVATE*/, f.as_raw_fd(), 0);
        if p as usize == usize::MAX {
            let r: &'static [u8] = Box::leak(std::fs::read("/data/graph.txt").unwrap().into_boxed_slice());
            r
        } else {
            std::slice::from_raw_parts(p as *const u8, fz)
        }
    };
    let mut hp = 0usize;
    let n = rd(b, &mut hp) as usize;
    let body = { let mut i = hp; while i < fz && b[i] != b'\n' { i += 1; } min(i + 1, fz) };
    let nc = max(nt * 2, max(1, fz >> 20));
    let span = fz - body;
    let ls = |x: usize| -> usize {
        if x <= body { return body; }
        if x >= fz { return fz; }
        let mut i = x;
        if b[i - 1] != b'\n' {
            while i < fz && b[i] != b'\n' { i += 1; }
            i = min(i + 1, fz);
        }
        i
    };
    let range = |k: usize| (ls(body + span * k / nc), ls(body + span * (k + 1) / nc));

    // ---- pass 1: out-degrees, and a bit per node that has an in-edge ----
    let mut off = vec![0u32; n + 1];
    let hin: Vec<AtomicU64> = (0..(n >> 6) + 1).map(|_| AtomicU64::new(0)).collect();
    {
        let deg = atomic_view(&mut off);
        let hin = &hin;
        dynp(nt, nc, |k| {
            let (s, e) = range(k);
            scan(b, s, e, |u, v, _| {
                if u != v {
                    deg[u as usize].fetch_add(1, Relaxed);
                    let w = &hin[(v >> 6) as usize];
                    let bit = 1u64 << (v & 63);
                    if w.load(Relaxed) & bit == 0 { w.fetch_or(bit, Relaxed); }
                }
            });
        });
    }
    let mut r = 0u32;
    for u in 0..n { r += off[u]; off[u] = r; } // inclusive prefix = end of each node's slice
    off[n] = r;
    let e_ = r as usize;

    // ---- pass 2: place every edge in its final slot; off[u] ends up as the start ----
    let mut adj = vec![0u64; e_];
    {
        let deg = atomic_view(&mut off);
        let ap = P(adj.as_mut_ptr());
        dynp(nt, nc, |k| {
            let (s, e) = range(k);
            scan(b, s, e, |u, v, w| {
                if u != v {
                    let pos = deg[u as usize].fetch_sub(1, Relaxed) - 1;
                    unsafe { ap.set(pos as usize, (v as u64) | ((w as u64) << 32)); }
                }
            });
        });
    }

    // ---- queries: answer the trivial ones now, in the original id space ----
    let mut qs: Vec<(u32, u32)> = Vec::new();
    {
        let qb = std::fs::read("/data/queries.txt").unwrap();
        let (mut p, e) = (0usize, qb.len());
        loop {
            while p < e && !qb[p].is_ascii_digit() { p += 1; }
            if p >= e { break; }
            let s = rd(&qb, &mut p) as u32;
            let t = rd(&qb, &mut p) as u32;
            qs.push((s, t));
        }
    }
    let nq = qs.len();
    let mut ans = vec![0i64; nq];
    let mut pend: Vec<u32> = Vec::new();
    for (i, &(s, t)) in qs.iter().enumerate() {
        let no_out = off[s as usize] == off[s as usize + 1];
        let no_in = ((hin[(t >> 6) as usize].load(Relaxed) >> (t & 63)) & 1) == 0;
        if s == t { ans[i] = 0; }
        else if no_out || no_in { ans[i] = -1; }
        else { pend.push(i as u32); }
    }
    drop(hin);

    // ---- renumber nodes in BFS order if ids look scattered ----
    let mut nid: Vec<u32> = Vec::new();
    let (mut nr, mut tot) = (0u64, 0u64);
    {
        let sd = max(1, n / 200000);
        let mut u = 0;
        while u < n {
            for i in off[u] as usize..off[u + 1] as usize {
                let v = adj[i] as u32 as usize;
                tot += 1;
                nr += (u.abs_diff(v) <= 4096) as u64;
            }
            u += sd;
        }
    }
    if tot > 0 && nr * 10 < tot * 6 && n > 1 {
        let vis: Vec<AtomicU8> = (0..n).map(|_| AtomicU8::new(0)).collect();
        let mut lists: Vec<Vec<u32>> = (0..=nt).map(|_| Vec::new()).collect();
        {
            let lp = P(lists.as_mut_ptr());
            let (offr, adjr, visr) = (&off[..], &adj[..], &vis[..]);
            par(nt, |k| {
                let q = unsafe { &mut *lp.at(k) };
                q.reserve(n / nt * 3 / 2 + 1024);
                let s = (n as u64 * k as u64 / nt as u64) as usize;
                if claim(visr, s) { q.push(s as u32); grow(q, offr, adjr, visr); }
            });
        }
        for r in 0..n {
            if claim(&vis, r) {
                lists[nt].push(r as u32);
                grow(&mut lists[nt], &off, &adj, &vis);
            }
        }
        let mut base = vec![0usize; nt + 2];
        for k in 0..=nt { base[k + 1] = base[k] + lists[k].len(); }
        nid = vec![0u32; n];
        {
            let np_ = P(nid.as_mut_ptr());
            let (lr, br) = (&lists, &base);
            par(nt + 1, |k| {
                for (j, &x) in lr[k].iter().enumerate() {
                    unsafe { np_.set(x as usize, (br[k] + j) as u32); }
                }
            });
        }
        drop(lists);
        drop(vis);

        // new offsets (indexed by new id)
        let bt = (n + nt - 1) / nt;
        let mut no = vec![0u32; n + 1];
        {
            let nop = P(no.as_mut_ptr());
            let (o, nr_) = (&off, &nid);
            par(nt, |k| {
                for u in k * bt..min(n, (k + 1) * bt) {
                    unsafe { nop.set(nr_[u] as usize + 1, o[u + 1] - o[u]); }
                }
            });
        }
        for i in 0..n { no[i + 1] += no[i]; }

        // copy edges: old nodes in order (sequential reads), release old pages as they are consumed
        let mut na = vec![0u64; e_];
        {
            let nap = P(na.as_mut_ptr());
            let oa = adj.as_ptr() as usize;
            let (o, a, nr_, nn) = (&off, &adj, &nid, &no);
            par(nt, |k| {
                let (a0, a1) = (k * bt, min(n, (k + 1) * bt));
                let mut u = a0;
                while u < a1 {
                    let ue = min(a1, u + 65536);
                    for x in u..ue {
                        let mut dst = nn[nr_[x] as usize] as usize;
                        for i in o[x] as usize..o[x + 1] as usize {
                            let e = a[i];
                            let to = nr_[e as u32 as usize] as u64;
                            unsafe { nap.set(dst, to | (e & 0xFFFF_FFFF_0000_0000)); }
                            dst += 1;
                        }
                    }
                    release(oa, o[u] as usize, o[ue] as usize);
                    u = ue;
                }
            });
        }
        off = no;
        adj = na;
        for &i in &pend {
            let q = &mut qs[i as usize];
            q.0 = nid[q.0 as usize];
            q.1 = nid[q.1 as usize];
        }
    }
    drop(nid);

    // ---- run the remaining queries ----
    pend.sort_unstable_by_key(|&i| qs[i as usize].0);
    let np = pend.len();
    {
        let ap = P(ans.as_mut_ptr());
        let nx = AtomicUsize::new(0);
        let (offr, adjr, qr, pr) = (&off, &adj, &qs, &pend);
        // Sorted queries go out in big contiguous chunks (each thread touches a compact slice of its
        // label array), shrinking towards the end so the threads finish together.
        par(min(nt, max(1, np)), |_| {
            let mut s = Sc { l: vec![0u64; n], t: Vec::new(), h: Heap { a: Vec::new() } };
            loop {
                let i = nx.load(Relaxed);
                if i >= np { break; }
                let c = if i < np * 2 / 3 { (3 * np + 4 * nt - 1) / (4 * nt) } else { max(2, (np - i) / (2 * nt)) };
                if nx.compare_exchange_weak(i, i + c, Relaxed, Relaxed).is_err() { continue; }
                for j in i..min(i + c, np) {
                    let q = pr[j] as usize;
                    let r = solve(offr, adjr, &mut s, qr[q].0, qr[q].1);
                    unsafe { ap.set(q, r); }
                }
            }
        });
    }
    let mut o = String::with_capacity(nq * 12);
    for a in &ans { o.push_str(&a.to_string()); o.push('\n'); }
    std::fs::write("/out/distances.txt", o).unwrap();
    std::process::exit(0);
}