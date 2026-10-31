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
struct Ent{u64 k;u32 v;};
static u32*off[2]; static Edge*adj[2];

static u64 num(const char*&p,const char*e){
  while(p<e&&(unsigned)(*p-'0')>9)++p;
  u64 x=0; while(p<e&&(unsigned)(*p-'0')<=9)x=x*10+*p++-'0';
  return x;
}

struct Heap{ // 4-ary min-heap, lazy deletion
  vector<Ent>a;
  void push(Ent e){
    size_t i=a.size(); a.push_back(e);
    for(;i&&a[(i-1)>>2].k>e.k;i=(i-1)>>2)a[i]=a[(i-1)>>2];
    a[i]=e;
  }
  void pop(){
    Ent e=a.back(); a.pop_back();
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

// Open-addressing label table: key = node id, value = dist+1 (0 = unlabeled).
struct Slot{u32 k;u32 pad;u64 v;};           // k==~0u means empty
struct HT{
  vector<Slot> s; size_t cnt=0,mask=0;
  HT(){init(1<<12);}
  void init(size_t c){s.assign(c,Slot{~0u,0,0});mask=c-1;cnt=0;}
  size_t ix(u32 x)const{return (size_t)(((u64)x*0x9E3779B97F4A7C15ULL)>>32)&mask;}
  u64 get(u32 x)const{
    for(size_t i=ix(x);;i=(i+1)&mask){
      if(s[i].k==x)return s[i].v;
      if(s[i].k==~0u)return 0;
    }
  }
  u64&at(u32 x){                              // inserts with value 0 if new; reference valid until next at()
    if((cnt+1)*2>s.size())grow();
    for(size_t i=ix(x);;i=(i+1)&mask){
      if(s[i].k==x)return s[i].v;
      if(s[i].k==~0u){s[i].k=x;++cnt;return s[i].v;}
    }
  }
  void grow(){
    vector<Slot>o; o.swap(s); init(o.size()*2);
    for(auto&e:o)if(e.k!=~0u)at(e.k)=e.v;
  }
  void clear(){
    if(s.size()>(1<<15))init(1<<12);
    else{fill(s.begin(),s.end(),Slot{~0u,0,0});cnt=0;}
  }
};

struct Sc{HT L[2];Heap h[2];};

static long long solve(Sc&S,u32 s,u32 t){
  if(s==t)return 0;
  if(off[0][s]==off[0][s+1]||off[1][t]==off[1][t+1])return -1;
  u64 mu=~0ULL; u32 st[2]={s,t};
  for(int d=0;d<2;++d){S.h[d].a.clear();S.L[d].at(st[d])=1;S.h[d].push({0,st[d]});}
  while(!S.h[0].a.empty()&&!S.h[1].a.empty()&&S.h[0].a[0].k+S.h[1].a[0].k<mu){
    int d=S.h[0].a.size()<=S.h[1].a.size()?0:1;
    Heap&h=S.h[d]; Ent e=h.a[0]; h.pop();
    HT&me=S.L[d],&ot=S.L[d^1]; const Edge*A=adj[d];
    for(u32 i=off[d][e.v];i<off[d][e.v+1];++i){
      u32 v=A[i].to; u64 nd=e.k+A[i].w,c=nd+1;
      u64&cur=me.at(v);
      if(!cur||c<cur){cur=c;h.push({nd,v});}
      u64 o=ot.get(v); if(o)mu=min(mu,nd+o-1);
    }
    while(!h.a.empty()&&me.get(h.a[0].v)<h.a[0].k+1)h.pop(); // keep the top valid
  }
  S.L[0].clear();S.L[1].clear();
  return mu==~0ULL?-1:(long long)mu;
}

int main(){
  unsigned NT=12;
  {cpu_set_t c; if(!sched_getaffinity(0,sizeof c,&c))NT=min<int>(NT,max(1,CPU_COUNT(&c)));}
  auto par=[&](size_t T,auto f){vector<thread>v;F(k,0,T)v.emplace_back(f,(unsigned)k);for(auto&t:v)t.join();};
  auto dyn=[&](size_t N,auto f){atomic<size_t>c{0};par(NT,[&](unsigned){size_t k;while((k=c++)<N)f(k);});};

  // ---- parse: ~1 MB chunks, each pread by whichever thread is free, aligned to line starts ----
  int fd=open("/data/graph.txt",O_RDONLY); struct stat sb; fstat(fd,&sb);
  u64 fz=sb.st_size; size_t n=0,NC=max<size_t>(NT,fz>>20);
  vector<vector<E3>> P(NC);
  dyn(NC,[&](size_t k){
    u64 r0=fz/NC*k,r1=k+1==NC?fz:fz/NC*(k+1),s=k?r0-1:0,e=k+1==NC?fz:min(fz,r1+256);
    vector<char>b(e-s);
    for(size_t o=0;o<b.size();){ssize_t r=pread(fd,b.data()+o,b.size()-o,s+o);if(r<=0)break;o+=r;}
    const char*p=b.data(),*q=p+b.size();
    auto nxt=[&](const char*x){while(x<q&&*x!='\n')++x;return x<q?x+1:q;};
    if(!k){const char*h=p;n=num(h,q);}
    const char*a=nxt(p),*z=k+1==NC?q:nxt(p+(r1-1-s));
    P[k].reserve((z-a)/12+16);
    for(;;){
      while(a<z&&(unsigned)(*a-'0')>9)++a;
      if(a>=z)break;
      u32 u=num(a,z),v=num(a,z),w=num(a,z);
      if(u!=v)P[k].push_back({u,v,w});
    }
  });

  // ---- CSR build: d=0 forward (keyed by u) from the parsed edges, d=1 reverse (keyed by v) from the forward CSR.
  // Edges are bucketed by node range (counting sort without atomics), then each bucket is built inside the cache. ----
  const int SH=15; size_t B=(n>>SH)+1,E=0,bk=(n+NC-1)/NC;
  auto each=[&](int d,size_t k,auto f){
    if(!d){for(auto&e:P[k])f(e);}
    else F(u,k*bk,min(n,(k+1)*bk))F(i,off[0][u],off[0][u+1])f(E3{{(u32)u,adj[0][i].to,adj[0][i].w}});
  };
  vector<size_t>C(NC*B),bs(B+1);
  F(d,0,2){
    fill(C.begin(),C.end(),0);
    dyn(NC,[&](size_t k){each(d,k,[&](const E3&e){++C[k*B+(e.a[d]>>SH)];});});
    size_t r=0; F(b,0,B){bs[b]=r;F(k,0,NC){size_t c=C[k*B+b];C[k*B+b]=r;r+=c;}} bs[B]=E=r;
    E3*tmp=new E3[E+1];
    dyn(NC,[&](size_t k){each(d,k,[&](const E3&e){tmp[C[k*B+(e.a[d]>>SH)]++]=e;});});
    if(!d){P.clear();P.shrink_to_fit();}
    off[d]=(u32*)malloc((n+1)*4); adj[d]=(Edge*)malloc((E+1)*8); off[d][n]=E;
    dyn(B,[&](size_t b){
      size_t lo=b<<SH,hi=min(n,(b+1)<<SH),i0=bs[b],i1=bs[b+1];
      vector<u32>c(hi>lo?hi-lo:0);
      F(i,i0,i1)++c[tmp[i].a[d]-lo];
      u32 r=i0; F(j,0,c.size()){u32 x=c[j];off[d][lo+j]=r;c[j]=r;r+=x;}
      F(i,i0,i1){auto&e=tmp[i];adj[d][c[e.a[d]-lo]++]={e.a[1-d],e.a[2]};}
    });
    delete[]tmp;
  }

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
      for(auto&q:L)ord.insert(ord.end(),q.begin(),q.end());
      size_t bt=(n+NT-1)/NT; nid.resize(n);
      par(NT,[&](unsigned k){F(i,k*bt,min(n,(k+1)*bt))nid[ord[i]]=i;});
      F(d,0,2){
        u32*no=(u32*)malloc((n+1)*4); Edge*na=(Edge*)malloc((E+1)*8); no[0]=0;
        par(NT,[&](unsigned k){F(i,k*bt,min(n,(k+1)*bt))no[i+1]=off[d][ord[i]+1]-off[d][ord[i]];});
        F(i,0,n)no[i+1]+=no[i];
        par(NT,[&](unsigned k){
          F(i,k*bt,min(n,(k+1)*bt)){u32 w=no[i],o=ord[i];F(j,off[d][o],off[d][o+1])na[w++]={nid[adj[d][j].to],adj[d][j].w};}
        });
        free(off[d]);free(adj[d]);off[d]=no;adj[d]=na;
      }
    }
  }

  // ---- queries ----
  vector<pair<u32,u32>>Q; {ifstream f("/data/queries.txt");u32 s,t;while(f>>s>>t){if(nid.size()){s=nid[s];t=nid[t];}Q.push_back({s,t});}}
  vector<long long>ans(Q.size()); vector<u32>od(Q.size()); iota(od.begin(),od.end(),0);
  sort(od.begin(),od.end(),[&](u32 a,u32 b){return Q[a].first<Q[b].first;});
  vector<u32>().swap(nid);
  // Sorted queries are handed out in big contiguous chunks, shrinking towards the end so the threads finish together.
  size_t N=Q.size(); atomic<size_t>nx{0};
  par(min<size_t>(NT,max<size_t>(1,N)),[&](unsigned){
    Sc S; // per-thread hash-table labels, no n-sized arrays
    for(size_t i=nx;i<N;i=nx){
      size_t c=i<N*2/3?(3*N+4*NT-1)/(4*NT):max<size_t>(2,(N-i)/(2*NT));
      if(!nx.compare_exchange_weak(i,i+c))continue;
      F(j,i,min(i+c,N)){u32 q=od[j];ans[q]=solve(S,Q[q].first,Q[q].second);}
    }
  });
  string o; for(auto a:ans)o+=to_string(a)+'\n';
  ofstream("/out/distances.txt")<<o;
  _exit(0);
}