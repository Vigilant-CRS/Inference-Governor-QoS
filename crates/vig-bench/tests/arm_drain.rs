//! Ein Messarm endet erst, wenn keine seiner Inferenzen mehr laeuft.
//!
//! Review vom 14.09., R03: `run_arm` wartete auf seine Kameraschleifen, nicht
//! auf die Aufrufe, die sie gestartet hatten, und schlief danach pauschal
//! 500 ms. Ein Arm kehrte deshalb zurueck, waehrend das Backend noch rechnete
//! — die naechste Messzelle mass dann neben fremder Arbeit, und ein
//! Shared-Memory-Puffer konnte erneut vergeben werden, waehrend sein Leser
//! noch lief.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use vig_bench::backend::{self, Backend};
use vig_bench::pilot::{self, ArmConfig, ArrivalRule, CameraDef, Sequence};
use vig_sim::workload::RuntimeDistribution;

fn core_ms(n: u64) -> vig_core::Duration {
    vig_core::Duration::from_millis(n).unwrap()
}

/// Eine Kamera ohne Annotation: hier zaehlt allein der Lebenszyklus.
fn camera(model: &str) -> CameraDef {
    CameraDef {
        name: "cam0".to_owned(),
        model: model.to_owned(),
        sequence: Arc::new(Sequence::from_frames_with(
            "drain",
            10,
            vec![Vec::new(); 10],
            ArrivalRule::FirstAppearance,
        )),
        frames: Arc::new(vec![128; 8 * 8 * 3 * 10]),
        size: 8,
        rate_hz: 10.0,
        regions: Vec::new(),
        input: ("input".to_owned(), "FP32".to_owned(), vec![1, 3, 8, 8]),
        target_classes: vec![0],
        iou_threshold: 0.5,
        classes: 1,
        ideal: None,
    }
}

/// Der Arm misst 100 ms, das Backend rechnet 2000 ms je Inferenz.
///
/// Kehrt `run_arm` zurueck, darf kein Slot des Backends mehr belegt sein.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_finished_arm_leaves_no_inference_running() {
    let times = HashMap::from([(
        "detector".to_owned(),
        RuntimeDistribution::constant(core_ms(2_000)),
    )]);
    let backend = Arc::new(Backend::new(1, times, 1));
    let endpoint = backend::start(Arc::clone(&backend)).await.to_string();

    // `run_arm` traegt die Messzelle im Future; auf dem Stack waere sie zu
    // gross (clippy::large_futures), wie bei den uebrigen Messwerkzeugen.
    let report = Box::pin(pilot::run_arm(ArmConfig {
        endpoint,
        via_governor: false,
        cameras: vec![camera("detector")],
        in_flight_cap: 1,
        duration: Duration::from_millis(100),
        llm: None,
    }))
    .await
    .unwrap();

    assert!(report.cameras[0].sent > 0, "nichts gesendet: {report:?}");
    assert_eq!(
        backend.slots.available_permits(),
        1,
        "der Arm kehrte zurueck, waehrend sein Backend noch rechnet: {report:?}"
    );
}
