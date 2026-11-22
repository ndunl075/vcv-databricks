# Test image with both binaries: /ssf is the entrypoint, /ss the general pipeline it hands off to.
# Build from the repo root:  docker buildx build --platform linux/amd64 --load -f fast/Dockerfile -t ssf:latest .
FROM rust:1-alpine AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked
WORKDIR /srcf
COPY fast/Cargo.toml fast/Cargo.lock ./
COPY fast/.cargo ./.cargo
COPY fast/src ./src
RUN cargo build --release --locked

FROM scratch
COPY --from=build /src/target/release/ss /ss
COPY --from=build /srcf/target/release/ssf /ssf
ENTRYPOINT ["/ssf"]
