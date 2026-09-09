# Zweistufiger Build: das Ergebnis ist ein einzelnes statisch gelinktes
# Binary in einem minimalen Image. Kein Rust-Toolchain, kein Cargo-Cache und
# keine Quellen im Auslieferungsartefakt.

FROM rust:1-bookworm AS build
WORKDIR /src

# protoc für die Codegenerierung. Das Build-Skript bevorzugt einen
# systemweiten protoc und greift sonst auf die mitgelieferte Binärversion
# zurück; hier ist der systemweite die gepflegte Variante.
RUN apt-get update \
 && apt-get install -y --no-install-recommends protobuf-compiler \
 && rm -rf /var/lib/apt/lists/*

COPY . .
RUN cargo build --release -p vig-cli

FROM debian:bookworm-slim
# `curl` ist fuer den Healthcheck da und fuer nichts anderes. Ein Image ohne
# jede Moeglichkeit, seinen eigenen Bereitschaftsendpunkt abzufragen, laesst
# sich in einem Rollout nicht sinnvoll pruefen.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

# Kein root: der Governor braucht keine Rechte über das hinaus, was ein
# Netzwerkdienst braucht (Spec 22.2).
RUN useradd --system --create-home --uid 10001 vig
USER vig

COPY --from=build /src/target/release/vig /usr/local/bin/vig
# 9001 nimmt Inferenzanfragen, 9090 traegt /metrics, /healthz und /readyz.
EXPOSE 9001 9090
ENTRYPOINT ["vig"]
CMD ["--help"]
