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
static u32*off[2]; static Edge*adj[2];

static u64 num(const char*&p,const char*e){
  while(p<e&&(unsigned)(*p-'0')>9)++p;
  u64 x=0; while(p<e&&(unsigned)(*p-'0')<=9)x=x*10+*p++-'0';
  return x;
}

// Search state is templated on the label width. u32 covers every distance below 2^32-1 (all road-network
// queries) with half the cache footprint; the rare query that would overflow is redone with u64.
template<class L>struct Ent{L k;u32 v;};
template<class L>struct Lb{L l[2];}; // fwd+bwd labels of one node side by side: one cache line per relaxation

template<class L>struct Heap{ // 4-ary min-heap, lazy deletion
  vector<Ent<L>>a;
  void push(Ent<L> e){
    size_t i=a.size(); a.push_back(e);
    for(;i&&a[(i-1)>>2].k>e.k;i=(i-1)>>2)a[i]=a[(i-1)>>2];
    a[i]=e;
  }
  void pop(){
    Ent<L> e=a.back(); a.pop_back();
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

template<class L>struct Sc{Lb<L>*lab=nullptr;vector<u32>T[2];Heap<L>h[2];}; // label = dist+1, 0 = unlabeled

// Bidirectional Dijkstra. Returns -1 if unreachable, -2 if a u32 label would overflow.
template<class L>static long long solve(Sc<L>&S,u32 s,u32 t){
  if(s==t)return 0;
  if(off[0][s]==off[0][s+1]||off[1][t]==off[1][t+1])return -1;
  u64 mu=~0ULL; u32 st[2]={s,t}; bool ov=false; Lb<L>*lab=S.lab;
  for(int d=0;d<2;++d){S.h[d].a.clear();lab[st[d]].l[d]=1;S.T[d].push_back(st[d]);S.h[d].push({0,st[d]});}
  while(!S.h[0].a.empty()&&!S.h[1].a.empty()&&(u64)S.h[0].a[0].k+S.h[1].a[0].k<mu){
    int d=S.h[0].a.size()<=S.h[1].a.size()?0:1;
    Heap<L>&h=S.h[d]; Ent<L> e=h.a[0]; h.pop();
    const Edge*A=adj[d];
    u32 i0=off[d][e.v],i1=off[d][e.v+1];
    for(u32 i=i0;i<i1;++i)__builtin_prefetch(lab+A[i].to);
    for(u32 i=i0;i<i1;++i){
      u64 nd=(u64)e.k+A[i].w;
      if(sizeof(L)==4&&nd>=0xFFFFFFFFull){ov=true;break;}
      u32 v=A[i].to; L c=(L)(nd+1); Lb<L>&x=lab[v]; L cur=x.l[d];
      if(!cur||c<cur){if(!cur)S.T[d].push_back(v);x.l[d]=c;h.push({(L)nd,v});}
      if(x.l[d^1])mu=min(mu,nd+x.l[d^1]-1);
    }
    if(ov)break;
    while(!h.a.empty()&&(u64)lab[h.a[0].v].l[d]<(u64)h.a[0].k+1)h.pop(); // keep the top valid
  }
  for(int d=0;d<2;++d){for(u32 v:S.T[d])lab[v].l[d]=0;S.T[d].clear();}
  if(ov)return -2;
  return mu==~0ULL?-1:(long long)mu;
}

int main(){
  unsigned NT=12;
  {cpu_set_t c; if(!sched_getaffinity(0,sizeof c,&c))NT=min<int>(NT,max(1,CPU_COUNT(&c)));}
  auto par=[&](size_t T,auto f){vector<thread>v;F(k,0,T)v.emplace_back(f,(unsigned)k);for(auto&t:v)t.join();};
  auto dyn=[&](size_t N,auto f){atomic<size_t>c{0};par(NT,[&](unsigned){size_t k;while((k=c++)<N)f(k);});};

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

  vector<pair<u32,u32>>Q; {ifstream f("/data/queries.txt");u32 s,t;while(f>>s>>t){if(nid.size()){s=nid[s];t=nid[t];}Q.push_back({s,t});}}
  vector<long long>ans(Q.size()); vector<u32>od(Q.size()); iota(od.begin(),od.end(),0);
  sort(od.begin(),od.end(),[&](u32 a,u32 b){return Q[a].first<Q[b].first;});
  vector<u32>().swap(nid);
  size_t N=Q.size(); atomic<size_t>nx{0};
  par(min<size_t>(NT,max<size_t>(1,N)),[&](unsigned){
    Sc<u32> S; S.lab=(Lb<u32>*)calloc(n,sizeof(Lb<u32>));
    Sc<u64> W; // wide fallback, allocated only if a query needs it
    for(size_t i=nx;i<N;i=nx){
      size_t c=i<N*2/3?(3*N+4*NT-1)/(4*NT):max<size_t>(2,(N-i)/(2*NT));
      if(!nx.compare_exchange_weak(i,i+c))continue;
      F(j,i,min(i+c,N)){
        u32 q=od[j]; long long r=solve(S,Q[q].first,Q[q].second);
        if(r==-2){if(!W.lab)W.lab=(Lb<u64>*)calloc(n,sizeof(Lb<u64>));r=solve(W,Q[q].first,Q[q].second);}
        ans[q]=r;
      }
    }
  });
  string o; for(auto a:ans)o+=to_string(a)+'\n';
  ofstream("/out/distances.txt")<<o;
  _exit(0);
}