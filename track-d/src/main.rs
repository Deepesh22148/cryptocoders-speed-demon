use std::cmp::{max, min};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering::Relaxed};
use std::thread;

const SH: usize = 15;

#[derive(Clone, Copy)]
struct Edge { to: u32, w: u32 }
#[derive(Clone, Copy)]
struct Ent { k: u64, v: u32 }

// Raw pointer that may be shared across threads; callers guarantee disjoint writes.
struct P<T>(*mut T);
impl<T> Clone for P<T> { fn clone(&self) -> Self { *self } }
impl<T> Copy for P<T> {}
unsafe impl<T> Send for P<T> {}
unsafe impl<T> Sync for P<T> {}
impl<T> P<T> {
    #[inline(always)] fn at(self, i: usize) -> *mut T { unsafe { self.0.add(i) } }
    #[inline(always)] unsafe fn set(self, i: usize, v: T) { std::ptr::write(self.0.add(i), v) }
}
impl<T: Copy> P<T> {
    #[inline(always)] unsafe fn get(self, i: usize) -> T { *self.0.add(i) }
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

fn num(b: &[u8], p: &mut usize, e: usize) -> u64 {
    while *p < e && !b[*p].is_ascii_digit() { *p += 1; }
    let mut x = 0u64;
    while *p < e && b[*p].is_ascii_digit() { x = x * 10 + (b[*p] - b'0') as u64; *p += 1; }
    x
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

struct G { off: [Vec<u32>; 2], adj: [Vec<Edge>; 2] }

// label = dist+1, 0 = unlabeled
struct Sc { l: [Vec<u64>; 2], t: [Vec<u32>; 2], h: [Heap; 2] }

fn solve(g: &G, s: &mut Sc, src: u32, dst: u32) -> i64 {
    if src == dst { return 0; }
    if g.off[0][src as usize] == g.off[0][src as usize + 1]
        || g.off[1][dst as usize] == g.off[1][dst as usize + 1] { return -1; }
    let mut mu = u64::MAX;
    let st = [src, dst];
    for d in 0..2 {
        s.h[d].a.clear();
        s.l[d][st[d] as usize] = 1;
        s.t[d].push(st[d]);
        s.h[d].push(Ent { k: 0, v: st[d] });
    }
    while !s.h[0].a.is_empty() && !s.h[1].a.is_empty() && s.h[0].a[0].k + s.h[1].a[0].k < mu {
        let d = if s.h[0].a.len() <= s.h[1].a.len() { 0 } else { 1 };
        let e = s.h[d].a[0];
        s.h[d].pop();
        let me = s.l[d].as_mut_ptr();
        let ot = s.l[1 - d].as_ptr();
        let a = &g.adj[d];
        let i0 = g.off[d][e.v as usize] as usize;
        let i1 = g.off[d][e.v as usize + 1] as usize;
        unsafe {
            for i in i0..i1 { pf(me.add(a[i].to as usize)); pf(ot.add(a[i].to as usize)); }
            for i in i0..i1 {
                let v = a[i].to as usize;
                let nd = e.k + a[i].w as u64;
                let c = nd + 1;
                let cur = *me.add(v);
                if cur == 0 || c < cur {
                    if cur == 0 { s.t[d].push(v as u32); }
                    *me.add(v) = c;
                    s.h[d].push(Ent { k: nd, v: v as u32 });
                }
                let ov = *ot.add(v);
                if ov != 0 { mu = mu.min(nd + ov - 1); }
            }
            // keep the top valid
            loop {
                let (k, v) = match s.h[d].a.first() { Some(x) => (x.k, x.v), None => break };
                if *me.add(v as usize) < k + 1 { s.h[d].pop(); } else { break; }
            }
        }
    }
    for d in 0..2 {
        for &v in &s.t[d] { s.l[d][v as usize] = 0; }
        s.t[d].clear();
    }
    if mu == u64::MAX { -1 } else { mu as i64 }
}

#[inline(always)]
fn each<F: FnMut([u32; 3])>(d: usize, k: usize, parts: &[Vec<[u32; 3]>], off0: &[u32], adj0: &[Edge],
                            bk: usize, n: usize, mut f: F) {
    if d == 0 {
        for &e in &parts[k] { f(e); }
    } else {
        for u in k * bk..min(n, (k + 1) * bk) {
            for i in off0[u] as usize..off0[u + 1] as usize {
                f([u as u32, adj0[i].to, adj0[i].w]);
            }
        }
    }
}

fn claim(vis: &[AtomicU8], v: usize) -> bool {
    vis[v].load(Relaxed) == 0 && vis[v].swap(1, Relaxed) == 0
}

fn grow(q: &mut Vec<u32>, off: &[Vec<u32>; 2], adj: &[Vec<Edge>; 2], vis: &[AtomicU8]) {
    let mut h = 0;
    while h < q.len() {
        let u = q[h] as usize;
        if h + 16 < q.len() { for d in 0..2 { pf(off[d].as_ptr().wrapping_add(q[h + 16] as usize)); } }
        if h + 8 < q.len() {
            for d in 0..2 { pf(adj[d].as_ptr().wrapping_add(off[d][q[h + 8] as usize] as usize)); }
        }
        for d in 0..2 {
            for i in off[d][u] as usize..off[d][u + 1] as usize {
                let v = adj[d][i].to;
                if claim(vis, v as usize) { q.push(v); }
            }
        }
        h += 1;
    }
}

fn main() {
    let nt = thread::available_parallelism().map(|x| x.get()).unwrap_or(12).min(12);

    // ---- parse: ~1 MB chunks, each read by whichever thread is free, aligned to line starts ----
    let f = File::open("/data/graph.txt").unwrap();
    let fz = f.metadata().unwrap().len() as usize;
    let nc = max(nt, fz >> 20);
    let mut parts: Vec<Vec<[u32; 3]>> = (0..nc).map(|_| Vec::new()).collect();
    let np = AtomicUsize::new(0);
    {
        let pp = P(parts.as_mut_ptr());
        dynp(nt, nc, |k| {
            let r0 = fz / nc * k;
            let r1 = if k + 1 == nc { fz } else { fz / nc * (k + 1) };
            let s = if k > 0 { r0 - 1 } else { 0 };
            let e = if k + 1 == nc { fz } else { min(fz, r1 + 256) };
            let mut b = vec![0u8; e - s];
            let mut o = 0;
            while o < b.len() {
                match f.read_at(&mut b[o..], (s + o) as u64) { Ok(0) | Err(_) => break, Ok(r) => o += r }
            }
            let q = b.len();
            let nxt = |mut x: usize| { while x < q && b[x] != b'\n' { x += 1; } if x < q { x + 1 } else { q } };
            if k == 0 { let mut h = 0; np.store(num(&b, &mut h, q) as usize, Relaxed); }
            let mut a = nxt(0);
            let z = if k + 1 == nc { q } else { nxt(r1 - 1 - s) };
            let v = unsafe { &mut *pp.at(k) };
            v.reserve((z.saturating_sub(a)) / 12 + 16);
            loop {
                while a < z && !b[a].is_ascii_digit() { a += 1; }
                if a >= z { break; }
                let u = num(&b, &mut a, z);
                let w1 = num(&b, &mut a, z);
                let w = num(&b, &mut a, z);
                if u != w1 { v.push([u as u32, w1 as u32, w as u32]); }
            }
        });
    }
    let n = np.load(Relaxed);

    // ---- CSR build: d=0 forward (keyed by u), d=1 reverse (keyed by v) from the forward CSR ----
    let b_ = (n >> SH) + 1;
    let bk = (n + nc - 1) / nc;
    let mut off: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
    let mut adj: [Vec<Edge>; 2] = [Vec::new(), Vec::new()];
    let mut c = vec![0usize; nc * b_];
    let mut bs = vec![0usize; b_ + 1];
    let mut e_ = 0usize;
    for d in 0..2 {
        c.fill(0);
        {
            let cp = P(c.as_mut_ptr());
            dynp(nt, nc, |k| {
                each(d, k, &parts, &off[0], &adj[0], bk, n, |e| unsafe {
                    *cp.at(k * b_ + (e[d] >> SH) as usize) += 1;
                });
            });
        }
        let mut r = 0usize;
        for b in 0..b_ {
            bs[b] = r;
            for k in 0..nc { let cc = c[k * b_ + b]; c[k * b_ + b] = r; r += cc; }
        }
        bs[b_] = r;
        e_ = r;
        // tmp is fully written below before being read
        let mut tmp: Vec<[u32; 3]> = Vec::with_capacity(e_ + 1);
        unsafe { tmp.set_len(e_ + 1); }
        {
            let cp = P(c.as_mut_ptr());
            let t = P(tmp.as_mut_ptr());
            dynp(nt, nc, |k| {
                each(d, k, &parts, &off[0], &adj[0], bk, n, |e| unsafe {
                    let slot = cp.at(k * b_ + (e[d] >> SH) as usize);
                    let pos = *slot;
                    *slot += 1;
                    t.set(pos, e);
                });
            });
        }
        if d == 0 { parts.clear(); parts.shrink_to_fit(); }
        let mut offd = vec![0u32; n + 1];
        offd[n] = e_ as u32;
        let mut adjd: Vec<Edge> = Vec::with_capacity(e_ + 1);
        unsafe { adjd.set_len(e_ + 1); }
        {
            let op = P(offd.as_mut_ptr());
            let ap = P(adjd.as_mut_ptr());
            let t = P(tmp.as_mut_ptr());
            let bs = &bs;
            dynp(nt, b_, |b| unsafe {
                let lo = b << SH;
                let hi = min(n, (b + 1) << SH);
                let (i0, i1) = (bs[b], bs[b + 1]);
                let mut cnt = vec![0u32; hi.saturating_sub(lo)];
                for i in i0..i1 { cnt[t.get(i)[d] as usize - lo] += 1; }
                let mut r = i0 as u32;
                for j in 0..cnt.len() { let x = cnt[j]; op.set(lo + j, r); cnt[j] = r; r += x; }
                for i in i0..i1 {
                    let e = t.get(i);
                    let slot = &mut cnt[e[d] as usize - lo];
                    ap.set(*slot as usize, Edge { to: e[1 - d], w: e[2] });
                    *slot += 1;
                }
            });
        }
        drop(tmp);
        off[d] = offd;
        adj[d] = adjd;
    }

    // ---- renumber nodes in BFS order (parallel region growing) if ids look scattered ----
    let mut nid: Vec<u32> = Vec::new();
    {
        let (mut nr, mut tot) = (0u64, 0u64);
        let sd = max(1, n / 200000);
        let mut u = 0;
        while u < n {
            for i in off[0][u] as usize..off[0][u + 1] as usize {
                let v = adj[0][i].to as usize;
                tot += 1;
                nr += ((if u > v { u - v } else { v - u }) <= 4096) as u64;
            }
            u += sd;
        }
        if tot > 0 && nr * 10 < tot * 6 && n > 1 {
            let vis: Vec<AtomicU8> = (0..n).map(|_| AtomicU8::new(0)).collect();
            let mut lists: Vec<Vec<u32>> = (0..=nt).map(|_| Vec::new()).collect();
            {
                let lp = P(lists.as_mut_ptr());
                let (offr, adjr, visr) = (&off, &adj, &vis[..]);
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
            let mut ord: Vec<u32> = Vec::with_capacity(n);
            for q in &lists { ord.extend_from_slice(q); }
            drop(lists);
            let bt = (n + nt - 1) / nt;
            nid = vec![0u32; n];
            {
                let np_ = P(nid.as_mut_ptr());
                let ord = &ord;
                par(nt, |k| { for i in k * bt..min(n, (k + 1) * bt) { unsafe { np_.set(ord[i] as usize, i as u32); } } });
            }
            for d in 0..2 {
                let mut no = vec![0u32; n + 1];
                {
                    let nop = P(no.as_mut_ptr());
                    let (o, ord) = (&off[d], &ord);
                    par(nt, |k| for i in k * bt..min(n, (k + 1) * bt) {
                        unsafe { nop.set(i + 1, o[ord[i] as usize + 1] - o[ord[i] as usize]); }
                    });
                }
                for i in 0..n { no[i + 1] += no[i]; }
                let mut na: Vec<Edge> = Vec::with_capacity(e_ + 1);
                unsafe { na.set_len(e_ + 1); }
                {
                    let nap = P(na.as_mut_ptr());
                    let (o, a, ord, nidr, no) = (&off[d], &adj[d], &ord, &nid, &no);
                    par(nt, |k| for i in k * bt..min(n, (k + 1) * bt) {
                        let mut w = no[i] as usize;
                        let oo = ord[i] as usize;
                        for j in o[oo] as usize..o[oo + 1] as usize {
                            unsafe { nap.set(w, Edge { to: nidr[a[j].to as usize], w: a[j].w }); }
                            w += 1;
                        }
                    });
                }
                off[d] = no;
                adj[d] = na;
            }
        }
    }
    let g = G { off, adj };

    // ---- queries ----
    let mut qs: Vec<(u32, u32)> = Vec::new();
    {
        let b = std::fs::read("/data/queries.txt").unwrap();
        let (mut p, e) = (0usize, b.len());
        loop {
            while p < e && !b[p].is_ascii_digit() { p += 1; }
            if p >= e { break; }
            let mut s = num(&b, &mut p, e) as u32;
            let mut t = num(&b, &mut p, e) as u32;
            if !nid.is_empty() { s = nid[s as usize]; t = nid[t as usize]; }
            qs.push((s, t));
        }
    }
    let nq = qs.len();
    let mut od: Vec<u32> = (0..nq as u32).collect();
    od.sort_unstable_by_key(|&i| qs[i as usize].0);
    drop(nid);
    let mut ans = vec![0i64; nq];
    {
        let ap = P(ans.as_mut_ptr());
        let nx = AtomicUsize::new(0);
        let (g, qs, od) = (&g, &qs, &od);
        // Sorted queries are handed out in big contiguous chunks, shrinking towards the end.
        par(min(nt, max(1, nq)), |_| {
            let mut s = Sc {
                l: [vec![0u64; n], vec![0u64; n]],
                t: [Vec::new(), Vec::new()],
                h: [Heap { a: Vec::new() }, Heap { a: Vec::new() }],
            };
            loop {
                let i = nx.load(Relaxed);
                if i >= nq { break; }
                let c = if i < nq * 2 / 3 { (3 * nq + 4 * nt - 1) / (4 * nt) } else { max(2, (nq - i) / (2 * nt)) };
                if nx.compare_exchange_weak(i, i + c, Relaxed, Relaxed).is_err() { continue; }
                for j in i..min(i + c, nq) {
                    let q = od[j] as usize;
                    let r = solve(g, &mut s, qs[q].0, qs[q].1);
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