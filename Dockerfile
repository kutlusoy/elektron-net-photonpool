FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin photond

FROM debian:bookworm-slim
RUN groupadd -r -g 10001 photon && useradd -r -u 10001 -g photon -d /app -s /usr/sbin/nologin photon
WORKDIR /app
RUN mkdir /data && chown photon:photon /data
COPY --from=build /src/target/release/photond /usr/local/bin/photond
COPY ui /app/ui
VOLUME ["/data"]
USER photon
EXPOSE 3333 8081 8082
ENTRYPOINT ["photond"]
CMD ["-c", "/config/photond.toml", "run"]
