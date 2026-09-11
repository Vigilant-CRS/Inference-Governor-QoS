//! Shared-Memory-Regionen fuer den Benchmark-Client.
//!
//! ADR-0003 hat gemessen, dass der gRPC-Copy-Pfad bei einem 6,2-MB-Frame 89 %
//! Zusatzaufwand kostet und als Shm-Referenz nur 2 %. Ein Gate-M3-Vergleich
//! ueber den Copy-Pfad wuerde deshalb den Transport messen und nicht das
//! Scheduling — und der Governor traegt diese Kosten doppelt, weil er zwei
//! Verbindungen bedient.
//!
//! Der Client legt die Tensordaten einmal ab und schickt danach nur noch
//! Verweise. Das ist zugleich die Konfiguration, die ein reales Deployment
//! ohnehin verwenden wuerde.
//!
//! ## Warum hier nichts abgebildet wird
//!
//! Der Client muss die Region **anlegen und fuellen**, nicht selbst lesen.
//! Abgebildet wird sie auf der anderen Seite, vom Backend. Damit kommt dieses
//! Modul mit gewoehnlicher Datei-Ein-/Ausgabe aus und braucht kein `mmap` —
//! und der Workspace behaelt sein ausnahmsloses `unsafe`-Verbot.

use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::path::PathBuf;

/// Groesse eines Schreibblocks beim Fuellen der Region.
const CHUNK: usize = 1 << 20;

/// Eine angelegte und gefuellte Shared-Memory-Region.
#[derive(Debug)]
pub struct Region {
    /// Der Name, unter dem die Region beim Backend registriert wird.
    pub name: String,
    /// Der POSIX-Schluessel, also der Pfad relativ zu `/dev/shm`.
    pub key: String,
    /// Die Groesse in Bytes.
    pub byte_size: u64,
    path: PathBuf,
}

impl Region {
    /// Legt eine Region an und fuellt sie mit Nullen.
    ///
    /// Nullen und keine Zufallsdaten: die Laufzeit der hier verwendeten Kernel
    /// haengt nicht vom Inhalt ab, und identische Eingaben machen zwei
    /// Vergleichslaeufe vergleichbar.
    ///
    /// # Errors
    ///
    /// Wenn die Datei unter `/dev/shm` nicht angelegt oder nicht gefuellt
    /// werden kann.
    pub fn create(name: &str, byte_size: u64) -> io::Result<Self> {
        let key = format!("/{name}");
        let path = PathBuf::from(format!("/dev/shm/{name}"));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;

        let zeros = vec![0_u8; CHUNK];
        let mut written = 0_u64;
        while written < byte_size {
            let remaining = usize::try_from(byte_size.saturating_sub(written)).unwrap_or(CHUNK);
            let take = remaining.min(CHUNK);
            file.write_all(zeros.get(..take).unwrap_or(&zeros))?;
            written = written.saturating_add(take as u64);
        }
        file.flush()?;

        Ok(Self {
            name: name.to_owned(),
            key,
            byte_size,
            path,
        })
    }
}

impl Region {
    /// Der Pfad unter `/dev/shm`.
    ///
    /// Wer echte Bilder statt Nullen schicken will — der Edge-Pilot —,
    /// schreibt sie hierhin, bevor er den Auftrag abschickt. Eine Region je
    /// gleichzeitig offenem Auftrag: wer ueberschreibt, waehrend das Backend
    /// noch liest, misst Datensalat.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        // Ein Benchmark, der /dev/shm zumuellt, wird beim naechsten Lauf zur
        // Fehlerquelle.
        let _ = std::fs::remove_file(&self.path);
    }
}
