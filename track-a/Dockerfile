# FROM gcc:15@sha256:eb5e64286ad47187b32bda75c6b6916166ecb41da56559b8b09e0001748d2cce AS build
# WORKDIR /src
# COPY main.cpp /src/main.cpp
# RUN g++ -O3 -march=native -std=c++23 -pthread -static -o solution main.cpp

# FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
# COPY --from=build /src/solution /app/solution
# ENTRYPOINT ["/app/solution"]

FROM rust:1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb AS build
WORKDIR /src
COPY . .
ENV RUSTFLAGS="-C target-cpu=native"
RUN cargo build --release --offline

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /src/target/release/solution /app/solution
ENTRYPOINT ["/app/solution"]