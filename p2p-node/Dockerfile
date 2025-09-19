FROM ghcr.io/rust-lang/rust:nightly-bookworm AS build
WORKDIR /usr/src/app
# With docker-compose build context at ./p2p-node, copy manifests from root of context
COPY Cargo.toml Cargo.lock ./
# If Cargo.lock is empty or undesired in container, remove it to allow Cargo to resolve fresh
RUN [ -s Cargo.lock ] || rm -f Cargo.lock
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs && cargo build --release --locked || true
COPY . ./
# Important: Ensure we don't accidentally ship the stub built above.
# In some Docker setups, host file mtimes can be older than the stub file we created,
# causing Cargo to think the crate is up-to-date and reuse the stub. Clean to force rebuild.
RUN cargo clean && cargo build --release --locked

FROM debian:bookworm-slim
ENV RUST_LOG=info
WORKDIR /app
COPY --from=build /usr/src/app/target/release/p2p-node /usr/local/bin/p2p-node
RUN useradd -ms /bin/bash app && mkdir -p /data && chown -R app:app /data
USER app
EXPOSE 8080/udp 8080
CMD ["/usr/local/bin/p2p-node"]
