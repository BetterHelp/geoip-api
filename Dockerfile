FROM ekidd/rust-musl-builder:latest AS build
COPY ./src ./src
COPY Cargo.toml Cargo.lock ./
ENV RUSTFLAGS='-C target-cpu=skylake'
RUN cargo build --release

FROM alpine:latest as certs
RUN apk --update add ca-certificates

FROM scratch
COPY --from=certs /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /home/rust/src/target/x86_64-unknown-linux-musl/release/geoip-api /
USER 1000
ENV RUST_LOG WARN
EXPOSE 3000/tcp
CMD ["/geoip-api"]
