#include <bits/stdc++.h>
#include <fcntl.h>
#include <sched.h>
#include <sys/stat.h>
#include <unistd.h>
using namespace std;
typedef uint32_t u32; typedef uint64_t u64;
#define F(i,a,b) for(size_t i=(a);i<(b);++i)
struct Edge{u32 to,w;};
struct E3{u32 a[3];};
template<class D> struct Ent{D k;u32 v;};
static u32*off[2]; static Edge*adj[2];

template<class Fn> static void par(size_t T,Fn f){
  vector<thread>v; F(k,0,T)v.emplace_back(f,(unsigned)k); for(auto&t:v)t.join();
}

static u64 num(const char*&p,const char*e){
  while(p<e&&(unsigned)(*p-'0')>9)++p;
  u64 x=0; while(p<e&&(unsigned)(*p-'0')<=9)x=x*10+*p++-'0';
  return x;
}

template<class D> struct Heap{ // 4-ary min-heap, lazy deletion
  vector<Ent<D>>a;
  void push(Ent<D> e){
    size_t i=a.size(); a.push_back(e);
    for(;i&&a[(i-1)>>2].k>e.k;i=(i-1)>>2)a[i]=a[(i-1)>>2];
    a[i]=e;
  }
  void pop(){
    Ent<D> e=a.back(); a.pop_back();
    size_t n=a.size(),i=0;
    if(!n)return;
    for(size_t c;(c=4*i+1)<n;){
      size_t b=c; for(size_t j=c+1;j<min(c+4,n);++j)if(a[j].k<a[b].k)b=j;
      if(a[b].k>=e.k)break;
      a[i]=a[b]; i=b;
    }
    a[i]=e;
  }
};

template<class D> struct Sc{D*L[2];vector<u32>T[2];Heap<D>h[2];}; // label = dist+1, 0 = unlabeled

template<class D> static long long solve(Sc<D>&S,u32 s,u32 t){
  if(s==t)return 0;
  if(off[0][s]==off[0][s+1]||off[1][t]==off[1][t+1])return -1;
  u64 mu=~0ULL; u32 st[2]={s,t};
  for(int d=0;d<2;++d){S.h[d].a.clear();S.L[d][st[d]]=1;S.T[d].push_back(st[d]);S.h[d].push({0,st[d]});}
  while(!S.h[0].a.empty()&&!S.h[1].a.empty()&&(u64)S.h[0].a[0].k+S.h[1].a[0].k<mu){
    int d=S.h[0].a.size()<=S.h[1].a.size()?0:1;
    Heap<D>&h=S.h[d]; Ent<D> e=h.a[0]; h.pop();
    D*me=S.L[d],*ot=S.L[d^1]; const Edge*A=adj[d];
    u32 i0=off[d][e.v],i1=off[d][e.v+1];
    for(u32 i=i0;i<i1;++i){__builtin_prefetch(me+A[i].to);__builtin_prefetch(ot+A[i].to);}
    for(u32 i=i0;i<i1;++i){
      u32 v=A[i].to; D nd=e.k+A[i].w,c=nd+1,cur=me[v];
      if(!cur||c<cur){if(!cur)S.T[d].push_back(v);me[v]=c;h.push({nd,v});}
      if(ot[v])mu=min(mu,(u64)nd+ot[v]-1);
    }
    while(!h.a.empty()&&me[h.a[0].v]<h.a[0].k+1)h.pop(); // keep the top valid
  }
  for(int d=0;d<2;++d){for(u32 v:S.T[d])S.L[d][v]=0;S.T[d].clear();}
  return mu==~0ULL?-1:(long long)mu;
}

template<class D> static void runQ(size_t n,const vector<pair<u32,u32>>&Q,vector<long long>&ans,
                                   const vector<u32>&od,unsigned NT){
  atomic<size_t>nx{0};
  par(min<size_t>(NT,max<size_t>(1,Q.size())),[&](unsigned){
    Sc<D> S; S.L[0]=(D*)calloc(n,sizeof(D)); S.L[1]=(D*)calloc(n,sizeof(D));
    size_t i;
    while((i=nx.fetch_add(2))<Q.size())F(j,i,min(i+2,Q.size())){u32 q=od[j];ans[q]=solve<D>(S,Q[q].first,Q[q].second);}
    free(S.L[0]);free(S.L[1]);
  });
}

int main(){
  unsigned NT=12;
  {cpu_set_t c; if(!sched_getaffinity(0,sizeof c,&c))NT=min<int>(NT,max(1,CPU_COUNT(&c)));}

  // ---- parse: each thread streams its byte range in 4MB chunks, aligned to line starts ----
  int fd=open("/data/graph.txt",O_RDONLY); struct stat sb; fstat(fd,&sb);
  u64 fz=sb.st_size; size_t n=0;
  vector<vector<E3>> P(NT);
  vector<u64> SW(NT,0),MW(NT,0);
  par(NT,[&](unsigned k){
    const u64 r0=fz/NT*k,r1=k+1==NT?fz:fz/NT*(k+1); const bool last=k+1==NT;
    const size_t CH=1<<22;
    u64 base=k?r0-1:0; size_t have=0; bool skip=true,fin=false;
    u64 sw=0,mw=0;
    vector<char>b(CH);
    P[k].reserve((r1-r0)/12+16);
    while(!fin){
      if(b.size()<have+CH)b.resize(have+CH);
      ssize_t r=pread(fd,b.data()+have,CH,base+have);
      bool eof=r<=0; if(r>0)have+=r;
      size_t cut=have;
      if(!eof){
        const char*nl=(const char*)memrchr(b.data(),'\n',have);
        cut=nl?nl-b.data()+1:0;
        if(!cut)continue; // need more data to finish a line
      }
      const char*a=b.data(),*z=b.data()+cut;
      if(skip){ // header line (thread 0) or partial line (others)
        const char*nl=(const char*)memchr(a,'\n',cut);
        if(!k){const char*h=a;n=num(h,nl?nl:z);}
        a=nl?nl+1:z; skip=false;
      }
      if(!last){
        if(r1==0){z=a;fin=true;}
        else if(base+cut>r1-1){ // first line start >= r1 lies in this region
          size_t o=r1-1-base;
          const char*nl=(const char*)memchr(b.data()+o,'\n',cut-o);
          z=nl?nl+1:b.data()+cut; fin=true;
        }
        if(z<a)z=a;
      }
      for(;;){
        while(a<z&&(unsigned)(*a-'0')>9)++a;
        if(a>=z)break;
        u32 u=num(a,z),v=num(a,z),w=num(a,z);
        if(u!=v){P[k].push_back({u,v,w});sw+=w;if(w>mw)mw=w;}
      }
      if(eof||fin)break;
      memmove(b.data(),b.data()+cut,have-cut); base+=cut; have-=cut;
    }
    SW[k]=sw;MW[k]=mw;
  });
  close(fd);

  // ---- CSR build ----
  const int SH=15; size_t B=(n>>SH)+1,E=0;
  vector<vector<size_t>>C(NT,vector<size_t>(B)); vector<size_t>bs(B+1);
  auto prefix=[&](){size_t r=0;F(b,0,B){bs[b]=r;F(k,0,NT){size_t c=C[k][b];C[k][b]=r;r+=c;}}bs[B]=r;return r;};
  auto build=[&](int d,E3*t){ // t is bucketed by key a[d]; builds off[d],adj[d], frees t
    off[d]=(u32*)malloc((n+1)*4); adj[d]=(Edge*)malloc((E+1)*8); off[d][n]=E;
    atomic<size_t>nb{0};
    par(NT,[&](unsigned){
      vector<u32>c; size_t b;
      while((b=nb++)<B){
        size_t lo=b<<SH,hi=min(n,(b+1)<<SH),i0=bs[b],i1=bs[b+1];
        c.assign(hi>lo?hi-lo:0,0);
        F(i,i0,i1)++c[t[i].a[d]-lo];
        u32 r=i0; F(j,0,c.size()){u32 x=c[j];off[d][lo+j]=r;c[j]=r;r+=x;}
        F(i,i0,i1){auto&e=t[i];adj[d][c[e.a[d]-lo]++]={e.a[1-d],e.a[2]};}
      }
    });
    delete[]t;
  };
  // forward: from P, bucketed by u
  par(NT,[&](unsigned k){for(auto&e:P[k])++C[k][e.a[0]>>SH];});
  E=prefix();
  {
    E3*t0=new E3[E+1];
    par(NT,[&](unsigned k){for(auto&e:P[k])t0[C[k][e.a[0]>>SH]++]=e; vector<E3>().swap(P[k]);});
    P.clear(); P.shrink_to_fit();
    build(0,t0);
  }
  // reverse: scatter from forward CSR, bucketed by v
  {
    vector<size_t>sp(NT+1); F(k,0,NT+1)sp[k]=E*k/NT;
    auto walk=[&](unsigned k,auto f){
      size_t e0=sp[k],e1=sp[k+1]; if(e0>=e1)return;
      size_t u=upper_bound(off[0],off[0]+n+1,(u32)e0)-off[0]-1;
      for(size_t i=e0;i<e1;++i){while(off[0][u+1]<=i)++u; f(u,adj[0][i]);}
    };
    F(k,0,NT)C[k].assign(B,0);
    par(NT,[&](unsigned k){walk(k,[&](size_t,const Edge&x){++C[k][x.to>>SH];});});
    prefix();
    E3*t1=new E3[E+1];
    par(NT,[&](unsigned k){walk(k,[&](size_t u,const Edge&x){
      E3 e; e.a[0]=(u32)u;e.a[1]=x.to;e.a[2]=x.w; t1[C[k][x.to>>SH]++]=e;});});
    build(1,t1);
  }
  C.clear(); C.shrink_to_fit();

  // ---- renumber nodes in BFS order (parallel region growing) if ids look scattered ----
  vector<u32>nid;
  {
    u64 nr=0,tot=0; size_t sd=max<size_t>(1,n/200000);
    for(size_t u=0;u<n;u+=sd)for(u32 i=off[0][u];i<off[0][u+1];++i){
      size_t v=adj[0][i].to; ++tot; nr+=(u>v?u-v:v-u)<=4096;
    }
    if(tot&&nr*10<tot*6&&n>1){
      vector<char>vis(n);
      auto claim=[&](u32 v){return !vis[v]&&!__atomic_exchange_n(&vis[v],1,__ATOMIC_RELAXED);};
      auto grow=[&](vector<u32>&q){
        for(size_t h=0;h<q.size();++h){
          u32 u=q[h];
          if(h+16<q.size())F(d,0,2)__builtin_prefetch(off[d]+q[h+16]);
          if(h+8<q.size())F(d,0,2)__builtin_prefetch(adj[d]+off[d][q[h+8]]);
          F(d,0,2)for(u32 i=off[d][u];i<off[d][u+1];++i)if(claim(adj[d][i].to))q.push_back(adj[d][i].to);
        }
      };
      vector<vector<u32>>L(NT+1);
      par(NT,[&](unsigned k){
        auto&q=L[k]; q.reserve(n/NT*3/2+1024);
        u32 s=(u64)n*k/NT; if(claim(s)){q.push_back(s);grow(q);}
      });
      F(r,0,n)if(claim(r)){L[NT].push_back(r);grow(L[NT]);}
      vector<u32>ord; ord.reserve(n);
      for(auto&q:L){ord.insert(ord.end(),q.begin(),q.end());vector<u32>().swap(q);}
      vector<char>().swap(vis);
      size_t bk=(n+NT-1)/NT; nid.resize(n);
      par(NT,[&](unsigned k){F(i,k*bk,min(n,(k+1)*bk))nid[ord[i]]=i;});
      F(d,0,2){
        u32*no=(u32*)malloc((n+1)*4); Edge*na=(Edge*)malloc((E+1)*8); no[0]=0;
        par(NT,[&](unsigned k){F(i,k*bk,min(n,(k+1)*bk))no[i+1]=off[d][ord[i]+1]-off[d][ord[i]];});
        F(i,0,n)no[i+1]+=no[i];
        par(NT,[&](unsigned k){
          F(i,k*bk,min(n,(k+1)*bk)){u32 w=no[i],o=ord[i];F(j,off[d][o],off[d][o+1])na[w++]={nid[adj[d][j].to],adj[d][j].w};}
        });
        free(off[d]);free(adj[d]);off[d]=no;adj[d]=na;
      }
    }
  }

  // ---- queries ----
  vector<pair<u32,u32>>Q; {ifstream f("/data/queries.txt");u32 s,t;while(f>>s>>t){if(nid.size()){s=nid[s];t=nid[t];}Q.push_back({s,t});}}
  vector<long long>ans(Q.size()); vector<u32>od(Q.size()); iota(od.begin(),od.end(),0);
  sort(od.begin(),od.end(),[&](u32 a,u32 b){return Q[a].first<Q[b].first;});

  // pick label width: any tentative dist <= min(sum w, maxw*n) + maxw, label adds 1
  u64 sum=0,mw=0; F(k,0,NT){sum+=SW[k];mw=max(mw,MW[k]);}
  u64 bound=min(sum,mw*(u64)n);
  if(bound+mw+2<0xFFFFFFFFULL)runQ<u32>(n,Q,ans,od,NT);
  else                        runQ<u64>(n,Q,ans,od,NT);

  string o; for(auto a:ans)o+=to_string(a)+'\n';
  ofstream("/out/distances.txt")<<o;
  _exit(0);
}