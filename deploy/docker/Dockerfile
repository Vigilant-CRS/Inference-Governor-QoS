# Zweistufiger Build: das Ergebnis ist ein einzelnes statisch gelinktes
# Binary in einem minimalen Image. Kein Rust-Toolchain, kein Cargo-Cache und
# keine Quellen im Auslieferungsartefakt.

# Basisimages per Digest: ein Tag kann sich unter demselben Namen aendern,
# ein Digest nicht. Dependabot (`.github/dependabot.yml`) schlaegt neue vor.
FROM rust:1-bookworm@sha256:9a73a5088750b4c95158ab26629c854c3d6fc4b173cb7bc8079ad252d8ed7bfa AS build
WORKDIR /src

# protoc für die Codegenerierung. Das Build-Skript bevorzugt einen
# systemweiten protoc und greift sonst auf die mitgelieferte Binärversion
# zurück; hier ist der systemweite die gepflegte Variante.
RUN apt-get update \
 && apt-get install -y --no-install-recommends protobuf-compiler \
 && rm -rf /var/lib/apt/lists/*

COPY . .
RUN cargo build --release -p vig-cli \
 && cargo build --release -p vig-bench --bin vig-fit

FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
# `curl` ist fuer den Healthcheck da und fuer nichts anderes. Ein Image ohne
# jede Moeglichkeit, seinen eigenen Bereitschaftsendpunkt abzufragen, laesst
# sich in einem Rollout nicht sinnvoll pruefen. Das ist ein bewusst
# akzeptiertes Werkzeug im Laufzeitimage (docs/security.md, N7): der Prozess
# laeuft ohne Root, ohne Capabilities und mit schreibgeschuetztem Dateisystem.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

# Kein root: der Governor braucht keine Rechte über das hinaus, was ein
# Netzwerkdienst braucht (Spec 22.2).
RUN useradd --system --create-home --uid 10001 vig
USER vig

COPY --from=build /src/target/release/vig /usr/local/bin/vig
# Neben `vig`, weil `vig autotune` es dort sucht (ADR-0044).
COPY --from=build /src/target/release/vig-fit /usr/local/bin/vig-fit
# 9001 nimmt Inferenzanfragen, 9090 traegt /metrics, /healthz und /readyz.
EXPOSE 9001 9090
ENTRYPOINT ["vig"]
CMD ["--help"]
