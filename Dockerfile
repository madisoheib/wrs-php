# Static musl build -> scratch (spec §7.1). Alpine's default target is musl and
# Rust links it statically, so the binary runs on `scratch` with no .so files.
FROM rust:alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM scratch
COPY --from=build /src/target/release/resonance /resonance
# Default config (change creds via a mounted /resonance.toml or RESONANCE_* env).
COPY resonance.toml.example /resonance.toml
EXPOSE 8080
ENTRYPOINT ["/resonance", "start", "--config", "/resonance.toml"]
