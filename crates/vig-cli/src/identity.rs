//! Das Zusammensetzen eines Profilmanifests (NV-03).
//!
//! ## Warum der Betreiber mitreden muss
//!
//! Ein Profil gilt fuer ein Artefakt auf einem Geraet unter einer Runtime bei
//! einer bestimmten Aufteilung. Von diesen vier Dingen ist ueber das
//! Inferenzprotokoll genau eines vollstaendig erreichbar: die Runtime, und
//! auch die nur, soweit der Server sie meldet. Die GPU kennt er nicht, den
//! Treiber nicht, die Instanzaufteilung nur intern, und welche Bytes er
//! geladen hat, sagt er ohnehin nicht.
//!
//! Deshalb gibt es hier Flags. Sie sind kein Ersatz fuer Messung, sondern die
//! Erklaerung des Betreibers, unter welchen Bedingungen er gemessen hat. Was
//! er nicht angibt, bleibt `unknown` — und ein Profil mit `unknown`-Feldern
//! ist nicht geprueft, sondern unbelegt. Die automatische Erfassung von
//! Geraet und Treiber ist ein eigenes Paket (NV-04); sie wird diese Flags
//! nicht ersetzen, sondern vorbelegen.

use clap::Args;
use std::path::PathBuf;
use vig_config::manifest::{
    ArtifactIdentity, DeviceIdentity, MANIFEST_REVISION, MeasurementBounds, ProfileManifest,
    ResourceLayout, RuntimeIdentity, ValidityDomain,
};

/// Die Angaben, die nur der Betreiber kennt.
#[derive(Debug, Clone, Args)]
pub(crate) struct IdentityArgs {
    /// Path to the Triton model repository, for the artifact digest.
    ///
    /// Without it the digest stays `unknown`, and an exchanged weight file
    /// under the same version number cannot be detected.
    #[arg(long, value_name = "PATH")]
    pub model_repository: Option<PathBuf>,
    /// GPU name as reported by the driver.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
    /// CUDA compute capability, e.g. 8.6.
    #[arg(long, value_name = "X.Y")]
    pub compute_capability: Option<String>,
    /// Device memory in MiB.
    #[arg(long, value_name = "MIB")]
    pub device_memory_mib: Option<u64>,
    /// Driver version.
    #[arg(long, value_name = "VERSION")]
    pub driver: Option<String>,
    /// Executing library version, e.g. "TensorRT 10.3.0".
    #[arg(long, value_name = "VERSION")]
    pub library_version: Option<String>,
    /// How the device is partitioned: exclusive, mps, mig:<profile>, timeslice.
    #[arg(long, value_name = "MODE")]
    pub partition: Option<String>,
    /// Model instances on the device during the measurement.
    #[arg(long, value_name = "N")]
    pub instances: Option<u32>,
    /// State of the Triton rate limiter: off or resources.
    #[arg(long, value_name = "STATE")]
    pub rate_limiter: Option<String>,
    /// Where the measurement inputs come from. Defaults to "zeros".
    #[arg(long, value_name = "NAME")]
    pub dataset: Option<String>,
    /// How many independent runs were merged into this profile.
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub independent_runs: u32,
    /// Which GPU to read device identity from when it is not given explicitly.
    #[arg(long, value_name = "INDEX", default_value_t = 0)]
    pub gpu_index: u32,
    /// Do not read device identity from the hardware.
    ///
    /// Without this, `vig` fills in GPU name, compute capability, driver and
    /// memory from `nvidia-smi` where you did not state them. What you state
    /// always wins.
    #[arg(long)]
    pub no_hardware_probe: bool,
    /// Up to how many concurrently active models the numbers are claimed valid.
    #[arg(long, value_name = "N")]
    pub valid_up_to_models: Option<u32>,
    /// Up to which serialised occupancy in percent the numbers are claimed valid.
    #[arg(long, value_name = "PCT")]
    pub valid_up_to_occupancy_pct: Option<u32>,
}

impl Default for IdentityArgs {
    fn default() -> Self {
        Self {
            model_repository: None,
            device: None,
            compute_capability: None,
            device_memory_mib: None,
            driver: None,
            library_version: None,
            partition: None,
            instances: None,
            rate_limiter: None,
            dataset: None,
            independent_runs: 1,
            gpu_index: 0,
            no_hardware_probe: true,
            valid_up_to_models: None,
            valid_up_to_occupancy_pct: None,
        }
    }
}

/// Was der Messvorgang selbst ueber sich weiss.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MeasuredUnder {
    /// Aufwaermlaeufe.
    pub warmup: u32,
    /// Gleichzeitig unterwegs.
    pub concurrency: u32,
    /// Batchgroesse.
    pub batch_size: u32,
}

/// Das Ergebnis der Artefaktbestimmung.
///
/// Getrennt von `ArtifactIdentity`, weil hier auch der Grund steht, warum es
/// **nicht** geklappt hat — der gehoert in die Ausgabe, nicht in die Datei.
pub(crate) enum ArtifactLookup {
    /// Der Digest wurde gebildet.
    Found(ArtifactIdentity),
    /// Kein Repository angegeben.
    NotRequested,
    /// Es wurde gesucht und nicht gefunden.
    Failed(String),
}

/// Belegt die Geraetefelder aus der Hardwarebeobachtung vor (NV-04).
///
/// Was der Betreiber ausdruecklich angegeben hat, bleibt stehen: eine
/// Beobachtung ist eine Beobachtung, aber sie weiss nicht, welche Karte
/// gemeint war, wenn mehrere im Rechner stecken. Eine ausdrueckliche Angabe
/// zu ueberschreiben waere deshalb genau der falsche Vorrang.
///
/// Gibt zurueck, was ergaenzt wurde — der Betreiber soll sehen, was das
/// Werkzeug fuer ihn ausgefuellt hat, statt es spaeter in der Datei zu
/// entdecken.
pub(crate) fn prefill_from_hardware(args: &mut IdentityArgs, gpu_index: u32) -> Vec<String> {
    use vig_platform::Collector as _;

    let mut filled = Vec::new();
    let mut collector = vig_platform::NvidiaSmi::default();
    let snapshot = match collector.snapshot() {
        Ok(s) => s,
        Err(reason) => {
            // Kein Abbruch und kein Rateschluss: ohne Beobachtung bleiben die
            // Felder `unknown`, und das ist eine ehrliche Aussage.
            eprintln!("    Hardware nicht beobachtbar ({reason}); Geraetefelder bleiben unknown");
            return filled;
        }
    };
    let Some(gpu) = snapshot.gpu(gpu_index) else {
        eprintln!("    Keine GPU mit Index {gpu_index}; Geraetefelder bleiben unknown");
        return filled;
    };

    let mut take = |target: &mut Option<String>, value: Option<&String>, name: &str| {
        if target.is_none()
            && let Some(value) = value
        {
            *target = Some(value.clone());
            filled.push(format!("{name}={value}"));
        }
    };
    take(&mut args.device, gpu.name.value(), "device");
    take(
        &mut args.compute_capability,
        gpu.compute_capability.value(),
        "compute_capability",
    );
    take(&mut args.driver, gpu.driver.value(), "driver");
    if args.device_memory_mib.is_none()
        && let Some(mib) = gpu.memory_total_mib.value()
    {
        args.device_memory_mib = Some(*mib);
        filled.push(format!("device_memory_mib={mib}"));
    }

    // Ein gedrosselter Zustand waehrend der Messung gehoert nicht ins
    // Manifest — er gehoert dem Betreiber gesagt, **bevor** er misst. Ein
    // Profil, das unter einem Leistungslimit entstand, beschreibt nicht die
    // Karte, sondern die Karte unter diesem Limit.
    let limiting = gpu.limiting_reasons();
    if !limiting.is_empty() {
        eprintln!(
            "    WARNUNG Die Karte ist waehrend der Messung gedrosselt: {limiting:?}. \n\
             \x20   Das gemessene Profil gilt dann nur fuer diesen Zustand."
        );
    }

    filled
}

/// Bestimmt den Artefakt-Digest eines Backendmodells.
///
/// Erwartet die Triton-Repositorystruktur `<repo>/<backend_model>/`.
pub(crate) fn artifact_of(
    args: &IdentityArgs,
    backend_model: &str,
    versions: &[String],
) -> ArtifactLookup {
    let Some(repository) = args.model_repository.as_ref() else {
        return ArtifactLookup::NotRequested;
    };
    let directory = repository.join(backend_model);
    match crate::artifact::digest_of(&directory) {
        Ok(found) => ArtifactLookup::Found(ArtifactIdentity {
            digest: Some(found.digest),
            source: Some(found.source),
            bytes: Some(found.bytes),
            versions: versions.to_vec(),
        }),
        Err(e) => ArtifactLookup::Failed(format!("{}: {e}", directory.display())),
    }
}

/// Setzt ein Manifest aus Beobachtung, Betreiberangaben und Messbedingungen
/// zusammen.
///
/// Nichts wird hier geraten. Ein Feld, zu dem keine der drei Quellen etwas
/// sagt, bleibt `None` und damit `unknown`.
pub(crate) fn assemble(
    observation: &vig_backend_triton::Observation,
    artifact: ArtifactIdentity,
    args: &IdentityArgs,
    under: MeasuredUnder,
    recorded_at: Option<String>,
) -> ProfileManifest {
    ProfileManifest {
        revision: MANIFEST_REVISION,
        recorded_at,
        artifact,
        runtime: RuntimeIdentity {
            server: observation.server.clone(),
            server_version: observation.server_version.clone(),
            platform: observation.platform.clone(),
            library_version: args.library_version.clone(),
        },
        device: DeviceIdentity {
            name: args.device.clone(),
            compute_capability: args.compute_capability.clone(),
            memory_mib: args.device_memory_mib,
            driver: args.driver.clone(),
        },
        resources: ResourceLayout {
            partition: args.partition.clone(),
            instances: args.instances,
            rate_limiter: args.rate_limiter.clone(),
        },
        measurement: MeasurementBounds {
            // `zeros` ist der Default, weil die Profiler mit Nulltensoren
            // messen. Das ist eine Tatsache ueber das Werkzeug, keine
            // Annahme ueber den Betrieb — und deshalb darf sie hier stehen.
            dataset: Some(args.dataset.clone().unwrap_or_else(|| "zeros".to_owned())),
            dataset_digest: None,
            batch_size: Some(under.batch_size),
            concurrency: Some(under.concurrency),
            warmup: Some(under.warmup),
            independent_runs: Some(args.independent_runs),
        },
        validity: ValidityDomain {
            max_concurrent_models: args.valid_up_to_models,
            max_occupancy_pct: args.valid_up_to_occupancy_pct,
            max_input_kib: None,
        },
    }
}

/// Der Zeitstempel fuer `recorded_at`, als RFC 3339 in UTC.
///
/// Von Hand aus der Unix-Zeit gerechnet: eine Datumsbibliothek nur fuer einen
/// Kommentarzeitstempel waere eine Abhaengigkeit zu viel. Schlaegt die
/// Systemuhr fehl, gibt es keinen Zeitstempel — geraten wird keiner.
pub(crate) fn now_rfc3339() -> Option<String> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let (days, rest) = (seconds.checked_div(86_400)?, seconds.checked_rem(86_400)?);
    let (hour, minute, second) = (
        rest.checked_div(3_600)?,
        rest.checked_div(60)?.checked_rem(60)?,
        rest.checked_rem(60)?,
    );
    let (year, month, day) = civil_from_days(days)?;
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

/// Tage seit 1970-01-01 in ein Kalenderdatum, nach Howard Hinnants
/// `civil_from_days`.
fn civil_from_days(days: u64) -> Option<(u64, u64, u64)> {
    // Verschiebung auf eine Epoche, die auf einen 400-Jahre-Zyklus faellt.
    let z = days.checked_add(719_468)?;
    let era = z.checked_div(146_097)?;
    let doe = z.checked_rem(146_097)?;
    let yoe = doe
        .checked_sub(doe.checked_div(1_460)?)?
        .checked_add(doe.checked_div(36_524)?)?
        .checked_sub(doe.checked_div(146_096)?)?
        .checked_div(365)?;
    let y = yoe.checked_add(era.checked_mul(400)?)?;
    let doy = doe.checked_sub(
        yoe.checked_mul(365)?
            .checked_add(yoe.checked_div(4)?)?
            .checked_sub(yoe.checked_div(100)?)?,
    )?;
    let mp = doy.checked_mul(5)?.checked_add(2)?.checked_div(153)?;
    let d = doy
        .checked_sub(mp.checked_mul(153)?.checked_add(2)?.checked_div(5)?)?
        .checked_add(1)?;
    let m = if mp < 10 {
        mp.checked_add(3)?
    } else {
        mp.checked_sub(9)?
    };
    let year = if m <= 2 { y.checked_add(1)? } else { y };
    Some((year, m, d))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_is_the_first_of_january_nineteen_seventy() {
        assert_eq!(civil_from_days(0), Some((1970, 1, 1)));
    }

    #[test]
    fn a_leap_day_is_a_leap_day() {
        // 2024-02-29 ist Tag 19782 seit der Epoche.
        assert_eq!(civil_from_days(19_782), Some((2024, 2, 29)));
    }

    #[test]
    fn a_century_that_is_not_a_leap_year_is_handled() {
        // 1900 war kein Schaltjahr, 2000 schon. 2000-03-01 = Tag 11017.
        assert_eq!(civil_from_days(11_017), Some((2000, 3, 1)));
    }

    #[test]
    fn the_timestamp_has_the_expected_shape() {
        let stamp = now_rfc3339().unwrap();
        assert_eq!(stamp.len(), 20, "{stamp}");
        assert!(stamp.ends_with('Z'));
        assert_eq!(stamp.as_bytes()[4], b'-');
        assert_eq!(stamp.as_bytes()[10], b'T');
    }

    #[test]
    fn what_the_operator_does_not_state_stays_unknown() {
        let observation = vig_backend_triton::Observation {
            server: Some("triton".to_owned()),
            server_version: Some("2.52.0".to_owned()),
            platform: Some("tensorrt_plan".to_owned()),
            versions: vec!["1".to_owned()],
        };
        let manifest = assemble(
            &observation,
            ArtifactIdentity::default(),
            &IdentityArgs::default(),
            MeasuredUnder {
                warmup: 20,
                concurrency: 1,
                batch_size: 1,
            },
            None,
        );
        assert_eq!(manifest.device.name, None);
        assert_eq!(manifest.artifact.digest, None);
        assert_eq!(manifest.resources.partition, None);
        assert_eq!(manifest.validity, ValidityDomain::default());
        assert_eq!(manifest.runtime.server.as_deref(), Some("triton"));
    }

    #[test]
    fn without_a_repository_no_digest_is_invented() {
        let lookup = artifact_of(&IdentityArgs::default(), "rfdetr", &["1".to_owned()]);
        assert!(matches!(lookup, ArtifactLookup::NotRequested));
    }

    #[test]
    fn a_missing_model_directory_is_reported_not_swallowed() {
        let args = IdentityArgs {
            model_repository: Some(PathBuf::from("/nonexistent-model-repository")),
            ..IdentityArgs::default()
        };
        let lookup = artifact_of(&args, "rfdetr", &[]);
        assert!(matches!(lookup, ArtifactLookup::Failed(_)));
    }
}
