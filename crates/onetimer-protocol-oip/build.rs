//! Erzeugt die gRPC-Typen aus den Open-Inference-Protocol-Definitionen.
//!
//! Die `.proto`-Dateien stammen unveraendert aus dem Triton-Projekt
//! (BSD-3-Clause, siehe `THIRD_PARTY_NOTICES.md`). Sie werden bewusst nicht
//! von Hand nachgebaut: Spec L-001 verlangt, dass ein Standardclient ohne
//! kundenspezifisches SDK inferieren kann, und das geht nur mit exakt der
//! Wire-Definition, die Triton und KServe verwenden.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protos = ["../../proto/oip/grpc_service.proto"];
    let includes = ["../../proto/oip"];

    let mut config = prost_build::Config::new();

    // WP0 verlangt, dass ein frischer Clone deterministisch baut. Ein
    // systemweit installierter `protoc` wird bevorzugt — er ist die vom
    // Betriebssystem gepflegte Variante. Fehlt er, greift die mitgelieferte
    // Binaerversion, damit weder CI noch ein neuer Entwickler erst ein
    // Systempaket installieren muss.
    //
    // Der Pfad wird direkt uebergeben und nicht ueber `PROTOC` gesetzt:
    // `std::env::set_var` ist seit Edition 2024 `unsafe`, und der Workspace
    // verbietet `unsafe` ausnahmslos.
    if std::env::var_os("PROTOC").is_none()
        && which_protoc().is_none()
        && let Ok(path) = protoc_bin_vendored::protoc_bin_path()
    {
        config.protoc_executable(path);
    }

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_with_config(config, &protos, &includes)?;

    for proto in protos {
        println!("cargo:rerun-if-changed={proto}");
    }
    println!("cargo:rerun-if-env-changed=PROTOC");
    Ok(())
}

/// Sucht `protoc` im `PATH`.
fn which_protoc() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("protoc"))
        .find(|candidate| candidate.is_file())
}
