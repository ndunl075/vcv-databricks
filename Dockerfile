# Shopping Sessions: static Rust binary in a scratch image.
# Base image pinned by digest (index of rust:1-alpine); the final rebuilds from this file.
FROM rust:1-alpine@sha256:8d0e82abfdf9a38071e07cabc5afba4ddbe077631a80e4e742a760c38beadc33 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM scratch
COPY --from=build /src/target/release/ss /ss
ENTRYPOINT ["/ss"]
