# syntax=docker/dockerfile:1
# Relais SMTP — image Docker
#   docker build -t relais-smtp .
#   docker run -d -p 2525:2525 -p 8080:8080 -v relais-smtp-data:/data relais-smtp

# ---------- compilation ----------
FROM rust:1-slim-bookworm AS build
WORKDIR /src

# Les dépendances d'abord, pour profiter du cache quand seul le code change.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src web \
 && echo "fn main() {}" > src/main.rs \
 && touch web/index.html \
 && cargo build --release --locked \
 && rm -rf src web

COPY src ./src
COPY web ./web
RUN touch src/main.rs && cargo build --release --locked

# ---------- image finale ----------
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends tzdata \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin relais \
 && mkdir -p /data && chown relais:relais /data

COPY --from=build /src/target/release/relais-smtp /usr/local/bin/relais-smtp

ENV RELAIS_SMTP_DIR=/data \
    RELAIS_CONTAINER=1 \
    SMTP_PORT=2525 \
    WEB_PORT=8080 \
    TZ=Europe/Paris

USER relais
VOLUME ["/data"]
EXPOSE 2525 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/relais-smtp", "--sante"]

LABEL org.opencontainers.image.title="Relais SMTP" \
      org.opencontainers.image.description="Serveur SMTP relais avec interface web, pour Dolibarr, Immich et autres applications"

ENTRYPOINT ["/usr/local/bin/relais-smtp"]
