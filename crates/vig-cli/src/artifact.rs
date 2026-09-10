//! Der Artefakt-Digest eines Backendmodells (NV-03).
//!
//! ## Warum das Dateisystem und nicht das Protokoll
//!
//! Ueber das Inferenzprotokoll meldet ein Server Namen, Versionen, Plattform
//! und Tensorformen — nicht aber, welche Bytes er geladen hat. Wer die
//! Gewichtsdatei unter derselben Versionsnummer austauscht, aendert kein
//! einziges dieser Felder. Das hinterlegte Laufzeitprofil gilt dann fuer ein
//! Modell, das nicht mehr laeuft, und nichts an der Metadatenlage verraet es.
//!
//! Der einzige Ort, an dem das sichtbar ist, ist das Modellrepository selbst.
//! Deshalb liest dieses Modul Dateien — und deshalb ist der Digest optional:
//! Wo der Governor das Repository nicht sieht (entfernter Server, Container
//! ohne gemeinsames Volume), bleibt das Feld `unknown`. Unbekannt ist kein
//! Fehler; es als "geprueft" auszugeben waere einer.
//!
//! ## Was in den Digest eingeht
//!
//! **Nur die Versionsverzeichnisse.** Ein Triton-Modellverzeichnis hat die
//! Form `<modell>/<version>/…`; das Artefakt liegt in den
//! Versionsunterverzeichnissen, alles auf der oberen Ebene ist Konfiguration
//! oder Dokumentation. Digestiert wird deshalb, was unter den rein
//! numerischen Unterverzeichnissen liegt — in sortierter Pfadreihenfolge,
//! jeweils mit laengenpraefixiertem relativem Pfad und Inhalt.
//!
//! Diese Regel ist eine Korrektur. Zuerst wurde alles ausser `config.pbtxt`
//! digestiert. Das ging solange gut, bis im Modellverzeichnis eine
//! `PROVENANCE.txt` lag: der Digest aenderte sich, weil jemand eine Notiz
//! bearbeitet hatte. Ein Artefaktdigest, den ein Kommentar verschiebt, ist
//! kein Artefaktdigest.
//!
//! `config.pbtxt` bleibt ebenfalls draussen — auch dort, wo es keine
//! Versionsverzeichnisse gibt und deshalb auf die obere Ebene
//! zurueckgefallen wird. Darin stehen Instanzanzahl, Batchgrenzen und
//! Rate-Limiter-Ressourcen: die *Aufteilung* des Geraets, nicht das Artefakt.
//! Sie hat im Manifest ein eigenes Feld (`resources`). Waere sie im
//! Artefakt-Digest, waere jede Umkonfiguration ein "anderes Modell".
//!
//! Die Laengenpraefixe sind kein Zierrat: ohne sie haetten die Dateipaare
//! (`ab`, `c`) und (`a`, `bc`) denselben Digest.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Dateien, die nicht zum Artefakt zaehlen.
const EXCLUDED: &[&str] = &["config.pbtxt"];

/// Wie viel je Leseoperation angefordert wird.
///
/// TensorRT-Plaene sind hunderte Megabyte gross; sie am Stueck in den Speicher
/// zu laden waere auf einem Edge-Geraet keine gute Idee.
const CHUNK: usize = 1 << 20;

/// Der Digest eines Modellartefakts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactDigest {
    /// Der Digest als `sha256:<hex>`.
    pub digest: String,
    /// Woraus er gebildet wurde.
    pub source: String,
    /// Die Gesamtgroesse der einbezogenen Dateien.
    pub bytes: u64,
    /// Wie viele Dateien einbezogen wurden.
    pub files: usize,
}

/// Bildet den Digest ueber ein Modellverzeichnis oder eine einzelne Datei.
///
/// # Errors
///
/// Wenn der Pfad nicht existiert oder nicht gelesen werden kann.
pub(crate) fn digest_of(path: &Path) -> io::Result<ArtifactDigest> {
    let metadata = std::fs::metadata(path)?;
    if metadata.is_file() {
        let mut hasher = Sha256::new();
        let bytes = absorb(&mut hasher, path, "")?;
        return Ok(ArtifactDigest {
            digest: finish(hasher),
            source: "file".to_owned(),
            bytes,
            files: 1,
        });
    }

    // Versionsverzeichnisse eines Triton-Modells: rein numerische Namen.
    let versions = version_directories(path)?;
    let mut entries = Vec::new();
    if versions.is_empty() {
        collect(path, Path::new(""), &mut entries)?;
    } else {
        for version in &versions {
            collect(path, version, &mut entries)?;
        }
    }
    entries.sort();

    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    for relative in &entries {
        let full = path.join(relative);
        let name = relative.to_string_lossy();
        bytes = bytes.saturating_add(absorb(&mut hasher, &full, &name)?);
    }

    Ok(ArtifactDigest {
        digest: finish(hasher),
        source: if versions.is_empty() {
            "directory".to_owned()
        } else {
            "versions".to_owned()
        },
        bytes,
        files: entries.len(),
    })
}

/// Die rein numerischen Unterverzeichnisse, sortiert.
///
/// Triton nennt Modellversionen so. Ein Verzeichnis mit einem anderen Namen
/// ist keine Version und gehoert nicht zum Artefakt — es koennte alles sein,
/// vom Notizordner bis zum Testdatensatz.
fn version_directories(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
            out.push(PathBuf::from(name));
        }
    }
    out.sort();
    Ok(out)
}

/// Sammelt die einzubeziehenden Dateien, relativ zur Wurzel.
fn collect(root: &Path, relative: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let name = entry.file_name();
        let child = relative.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect(root, &child, out)?;
            continue;
        }
        // Symlinks werden nicht verfolgt: ein Digest, der aus dem
        // Modellverzeichnis hinausfuehrt, waere nicht mehr reproduzierbar.
        if !file_type.is_file() {
            continue;
        }
        if EXCLUDED.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        out.push(child);
    }
    Ok(())
}

/// Nimmt Pfad und Inhalt einer Datei in den Hash auf; gibt die Groesse zurueck.
fn absorb(hasher: &mut Sha256, path: &Path, name: &str) -> io::Result<u64> {
    let name_bytes = name.as_bytes();
    hasher.update(
        u64::try_from(name_bytes.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(name_bytes);

    let mut file = File::open(path)?;
    let mut buffer = vec![0_u8; CHUNK];
    let mut total = 0_u64;
    // Die Laenge steht *nach* dem Inhalt, weil sie erst dann feststeht, ohne
    // die Datei zweimal zu lesen. Sie ueberhaupt aufzunehmen ist der Punkt.
    let mut body = Sha256::new();
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let Some(chunk) = buffer.get(..read) else {
            break;
        };
        body.update(chunk);
        total = total.saturating_add(u64::try_from(read).unwrap_or(0));
    }
    hasher.update(total.to_le_bytes());
    hasher.update(body.finalize());
    Ok(total)
}

fn finish(hasher: Sha256) -> String {
    let bytes = hasher.finalize();
    let mut hex = String::with_capacity(71);
    hex.push_str("sha256:");
    for byte in bytes {
        use core::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vig-artifact-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_same_directory_yields_the_same_digest() {
        let dir = scratch("stable");
        std::fs::write(dir.join("model.plan"), b"weights").unwrap();
        let a = digest_of(&dir).unwrap();
        let b = digest_of(&dir).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.files, 1);
        assert_eq!(a.bytes, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exchanged_weights_change_the_digest() {
        let dir = scratch("weights");
        std::fs::write(dir.join("model.plan"), b"weights-a").unwrap();
        let before = digest_of(&dir).unwrap();
        std::fs::write(dir.join("model.plan"), b"weights-b").unwrap();
        let after = digest_of(&dir).unwrap();
        assert_ne!(
            before.digest, after.digest,
            "genau dieser Fall ist der Grund fuer NV-03"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_deployment_configuration_is_not_part_of_the_artifact() {
        let dir = scratch("config");
        std::fs::write(dir.join("model.plan"), b"weights").unwrap();
        let before = digest_of(&dir).unwrap();
        std::fs::write(dir.join("config.pbtxt"), b"instance_group { count: 2 }").unwrap();
        let after = digest_of(&dir).unwrap();
        assert_eq!(
            before.digest, after.digest,
            "die Aufteilung des Geraets hat im Manifest ein eigenes Feld"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_note_beside_the_version_directory_does_not_change_the_digest() {
        // Der Fehler, der das ausgeloest hat: eine `PROVENANCE.txt` im
        // Modellverzeichnis verschob den Digest. Ein Artefaktdigest, den ein
        // Kommentar bewegt, ist kein Artefaktdigest.
        let dir = scratch("note");
        std::fs::create_dir_all(dir.join("1")).unwrap();
        std::fs::write(dir.join("1/model.onnx"), b"weights").unwrap();
        std::fs::write(dir.join("config.pbtxt"), b"name: \"x\"").unwrap();
        let before = digest_of(&dir).unwrap();
        assert_eq!(before.source, "versions");

        std::fs::write(dir.join("PROVENANCE.txt"), b"Quelle: irgendwo").unwrap();
        let after = digest_of(&dir).unwrap();
        assert_eq!(before.digest, after.digest);
        assert_eq!(before.bytes, after.bytes, "die Notiz zaehlt auch nicht mit");

        std::fs::write(dir.join("PROVENANCE.txt"), b"Quelle: woanders").unwrap();
        assert_eq!(digest_of(&dir).unwrap().digest, before.digest);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_model_version_changes_the_digest() {
        let dir = scratch("newversion");
        std::fs::create_dir_all(dir.join("1")).unwrap();
        std::fs::write(dir.join("1/model.onnx"), b"weights").unwrap();
        let one = digest_of(&dir).unwrap();
        std::fs::create_dir_all(dir.join("2")).unwrap();
        std::fs::write(dir.join("2/model.onnx"), b"weights").unwrap();
        let two = digest_of(&dir).unwrap();
        assert_ne!(
            one.digest, two.digest,
            "eine zweite Version ist ein anderes Artefakt"
        );
        assert_eq!(two.files, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_non_numeric_directory_is_not_a_version() {
        let dir = scratch("notaversion");
        std::fs::create_dir_all(dir.join("1")).unwrap();
        std::fs::write(dir.join("1/model.onnx"), b"weights").unwrap();
        let before = digest_of(&dir).unwrap();
        std::fs::create_dir_all(dir.join("notizen")).unwrap();
        std::fs::write(dir.join("notizen/egal.txt"), b"viel Text").unwrap();
        assert_eq!(digest_of(&dir).unwrap().digest, before.digest);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_version_directories_the_old_rule_applies() {
        let dir = scratch("flat");
        std::fs::write(dir.join("model.onnx"), b"weights").unwrap();
        std::fs::write(dir.join("config.pbtxt"), b"name: \"x\"").unwrap();
        let d = digest_of(&dir).unwrap();
        assert_eq!(d.source, "directory");
        assert_eq!(d.files, 1, "die Konfiguration bleibt draussen");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_moved_file_changes_the_digest() {
        let dir = scratch("moved");
        std::fs::create_dir_all(dir.join("1")).unwrap();
        std::fs::write(dir.join("1/model.plan"), b"weights").unwrap();
        let before = digest_of(&dir).unwrap();
        std::fs::remove_dir_all(dir.join("1")).unwrap();
        std::fs::create_dir_all(dir.join("2")).unwrap();
        std::fs::write(dir.join("2/model.plan"), b"weights").unwrap();
        let after = digest_of(&dir).unwrap();
        assert_ne!(
            before.digest, after.digest,
            "eine neue Version ist ein neues Artefakt"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_shifted_boundary_does_not_collide() {
        let a = scratch("shift-a");
        std::fs::write(a.join("x"), b"ab").unwrap();
        std::fs::write(a.join("y"), b"c").unwrap();
        let left = digest_of(&a).unwrap();
        let b = scratch("shift-b");
        std::fs::write(b.join("x"), b"a").unwrap();
        std::fs::write(b.join("y"), b"bc").unwrap();
        let right = digest_of(&b).unwrap();
        assert_ne!(left.digest, right.digest);
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}
