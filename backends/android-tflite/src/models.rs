//! Ein Modell, ein Thread.
//!
//! Jedes Modell bekommt einen eigenen Betriebssystem-Thread, der seinen
//! Interpreter anlegt, besitzt und benutzt. Das hat zwei Gruende:
//!
//! * Der GL-Delegate bindet seinen Kontext an den Thread, der ihn anlegt.
//! * Es entspricht Triton mit einer Instanz je Modell: verschiedene Modelle
//!   laufen nebeneinander, Auftraege an dasselbe Modell warten in einer
//!   FIFO-Schlange. Der Vergleich "Backend direkt" gegen "ueber den Governor"
//!   bleibt damit derselbe wie in Gate M3.

use crate::tflite::ModelInfo;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::oneshot;

/// Was ein Modellthread ausfuehrt: Eingaben hinein, Ausgaben heraus.
pub(crate) type Runner = Box<dyn FnMut(&[Vec<u8>]) -> Result<Vec<Vec<u8>>, String>>;

/// Eine erledigte Inferenz.
#[derive(Debug)]
pub(crate) struct Done {
    pub(crate) outputs: Vec<Vec<u8>>,
    /// Zeit in der Schlange des Modellthreads.
    pub(crate) queue: Duration,
    /// Zeit im Interpreter (Kopie hinein, Invoke, Kopie heraus).
    pub(crate) compute: Duration,
}

struct Job {
    inputs: Vec<Vec<u8>>,
    enqueued: Instant,
    reply: oneshot::Sender<Result<Done, String>>,
}

/// Die Statistik eines Modells, wie OIP sie abfragt.
///
/// Der Abschlusszaehler ist der Nachweis, mit dem der Governor einen
/// gehaltenen Slotkredit zurueckgibt (NV-00): er steigt erst, **nachdem** der
/// Interpreter fertig ist, und zwar im Modellthread selbst. Ob noch jemand auf
/// die Antwort wartet, spielt dafuer keine Rolle (ADR-0042).
#[derive(Debug, Default)]
pub(crate) struct Stats {
    pub(crate) success: AtomicU64,
    pub(crate) success_ns: AtomicU64,
    pub(crate) fail: AtomicU64,
    pub(crate) fail_ns: AtomicU64,
    pub(crate) queue_ns: AtomicU64,
    pub(crate) compute_ns: AtomicU64,
    /// Wanduhr der letzten Anfrage in ms seit Epoch, wie bei Triton.
    pub(crate) last_inference_ms: AtomicU64,
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl Stats {
    pub(crate) fn record(&self, result: &Result<Done, String>, total: Duration) {
        self.last_inference_ms.store(now_ms(), Ordering::Relaxed);
        if let Ok(done) = result {
            self.success_ns.fetch_add(nanos(total), Ordering::Relaxed);
            self.queue_ns
                .fetch_add(nanos(done.queue), Ordering::Relaxed);
            self.compute_ns
                .fetch_add(nanos(done.compute), Ordering::Relaxed);
            // Zuletzt: wer den Zaehler steigen sieht, sieht auch die Zeiten.
            self.success.fetch_add(1, Ordering::Release);
        } else {
            self.fail_ns.fetch_add(nanos(total), Ordering::Relaxed);
            self.fail.fetch_add(1, Ordering::Release);
        }
    }
}

/// Ein geladenes Modell.
#[derive(Debug)]
pub(crate) struct ModelHandle {
    pub(crate) info: ModelInfo,
    pub(crate) stats: Arc<Stats>,
    tx: std::sync::mpsc::Sender<Job>,
}

impl ModelHandle {
    /// Startet den Modellthread; `loader` laeuft **auf** diesem Thread.
    ///
    /// Kehrt erst zurueck, wenn das Modell geladen ist oder das Laden
    /// gescheitert ist.
    ///
    /// # Errors
    ///
    /// Die Meldung des Loaders, oder wenn der Thread nicht startet.
    pub(crate) fn spawn<L>(name: &str, loader: L) -> Result<Self, String>
    where
        L: FnOnce() -> Result<(ModelInfo, Runner), String> + Send + 'static,
    {
        // Unbeschraenkt, wie die Schlange einer Triton-Instanz: der Arm
        // "Backend direkt" soll Ueberlast als Wartezeit zeigen, nicht als
        // Ablehnung. Begrenzt wird davor, im Governor.
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let (report, outcome) = std::sync::mpsc::channel::<Result<ModelInfo, String>>();
        let stats = Arc::<Stats>::default();
        let thread_stats = Arc::clone(&stats);
        std::thread::Builder::new()
            .name(format!("tflite-{name}"))
            .spawn(move || {
                let (info, mut runner) = match loader() {
                    Ok(model) => model,
                    Err(error) => {
                        let _ = report.send(Err(error));
                        return;
                    }
                };
                if report.send(Ok(info)).is_err() {
                    return;
                }
                while let Ok(job) = rx.recv() {
                    let start = Instant::now();
                    let outputs = runner(&job.inputs);
                    let end = Instant::now();
                    let result = outputs.map(|outputs| Done {
                        outputs,
                        queue: start.saturating_duration_since(job.enqueued),
                        compute: end.saturating_duration_since(start),
                    });
                    // Erst zaehlen, dann zustellen: ist der Aufrufer schon
                    // abgebrochen, hat die GPU trotzdem gerechnet, und genau
                    // dieses Ende braucht der Governor als Nachweis.
                    thread_stats.record(&result, end.saturating_duration_since(job.enqueued));
                    let _ = job.reply.send(result);
                }
            })
            .map_err(|e| format!("{name}: Thread startet nicht: {e}"))?;
        let info = outcome
            .recv()
            .map_err(|_| format!("{name}: Modellthread endete beim Laden"))??;
        Ok(Self { info, stats, tx })
    }

    /// Reiht eine Inferenz ein und wartet auf ihr Ende.
    ///
    /// Gezaehlt wird im Modellthread; wird dieses Future abgebrochen, zaehlt
    /// der Auftrag trotzdem, sobald er gerechnet ist.
    ///
    /// # Errors
    ///
    /// Die Meldung des Interpreters, oder wenn der Modellthread beendet ist.
    pub(crate) async fn infer(&self, inputs: Vec<Vec<u8>>) -> Result<Done, String> {
        let started = Instant::now();
        let (reply, rx) = oneshot::channel();
        let job = Job {
            inputs,
            enqueued: started,
            reply,
        };
        if self.tx.send(job).is_ok()
            && let Ok(result) = rx.await
        {
            return result;
        }
        // Kein Modellthread mehr, der zaehlen koennte: der Auftrag ist nie
        // gerechnet worden und zaehlt hier als Fehler, wie bisher.
        let result = Err("Modellthread beendet".to_owned());
        self.stats.record(&result, started.elapsed());
        result
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;
    use crate::tflite::TensorInfo;

    fn tensor(name: &str, datatype: &'static str, shape: &[i64], bytes: usize) -> TensorInfo {
        TensorInfo {
            name: name.to_owned(),
            datatype,
            shape: shape.to_vec(),
            byte_size: bytes,
        }
    }

    #[tokio::test]
    async fn a_model_answers_on_its_own_thread_and_counts_after_finishing() {
        let info = ModelInfo {
            inputs: vec![tensor("in", "UINT8", &[1, 4], 4)],
            outputs: vec![tensor("out", "FP32", &[1, 1], 4)],
        };
        let handle = ModelHandle::spawn("m", move || {
            let runner: Runner = Box::new(|inputs: &[Vec<u8>]| {
                assert_eq!(std::thread::current().name(), Some("tflite-m"));
                Ok(vec![vec![inputs[0].iter().copied().sum::<u8>(); 4]])
            });
            Ok((info, runner))
        })
        .unwrap();
        let done = handle.infer(vec![vec![1, 2, 3, 4]]).await.unwrap();
        assert_eq!(done.outputs, vec![vec![10_u8; 4]]);
        assert_eq!(handle.stats.success.load(Ordering::Acquire), 1);
        assert!(handle.stats.last_inference_ms.load(Ordering::Relaxed) > 0);
    }

    #[tokio::test]
    async fn a_failed_inference_is_counted_as_failed() {
        let handle = ModelHandle::spawn("m", || {
            let runner: Runner = Box::new(|_: &[Vec<u8>]| Err("kaputt".to_owned()));
            Ok((ModelInfo::default(), runner))
        })
        .unwrap();
        assert_eq!(handle.infer(Vec::new()).await.unwrap_err(), "kaputt");
        assert_eq!(handle.stats.fail.load(Ordering::Acquire), 1);
        assert_eq!(handle.stats.success.load(Ordering::Acquire), 0);
    }

    #[test]
    fn a_model_that_does_not_load_reports_why() {
        let error = ModelHandle::spawn("m", || Err("Delegate abgelehnt".to_owned())).unwrap_err();
        assert_eq!(error, "Delegate abgelehnt");
    }

    #[tokio::test]
    async fn requests_to_one_model_queue_behind_each_other() {
        // Eine Instanz je Modell, wie Triton: der zweite Auftrag wartet.
        let handle = Arc::new(
            ModelHandle::spawn("m", || {
                let runner: Runner = Box::new(|_: &[Vec<u8>]| {
                    std::thread::sleep(Duration::from_millis(30));
                    Ok(Vec::new())
                });
                Ok((ModelInfo::default(), runner))
            })
            .unwrap(),
        );
        let a = tokio::spawn({
            let h = Arc::clone(&handle);
            async move { h.infer(Vec::new()).await.unwrap() }
        });
        let b = tokio::spawn({
            let h = Arc::clone(&handle);
            async move { h.infer(Vec::new()).await.unwrap() }
        });
        let (a, b) = (a.await.unwrap(), b.await.unwrap());
        let waited = a.queue.max(b.queue);
        assert!(waited >= Duration::from_millis(20), "gewartet: {waited:?}");
    }

    /// Bricht der wartende Aufruf ab, rechnet der Modellthread trotzdem zu
    /// Ende. Dieses Ende muss im Abschlusszaehler stehen, sonst fehlt dem
    /// Governor der Nachweis und der Slotkredit bleibt gehalten (ADR-0042).
    #[tokio::test]
    async fn a_cancelled_call_still_counts_the_job_that_finished() {
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let handle = Arc::new(
            ModelHandle::spawn("m", move || {
                let mut started = Some(started_tx);
                let runner: Runner = Box::new(move |_: &[Vec<u8>]| {
                    // Nur der erste Auftrag haelt an, bis der Test ihn freigibt.
                    if let Some(tx) = started.take() {
                        let _ = tx.send(());
                        let _ = release_rx.recv();
                    }
                    Ok(Vec::new())
                });
                Ok((ModelInfo::default(), runner))
            })
            .unwrap(),
        );
        let first = tokio::spawn({
            let h = Arc::clone(&handle);
            async move { h.infer(Vec::new()).await }
        });
        started_rx.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        release_tx.send(()).unwrap();
        handle.infer(Vec::new()).await.unwrap();
        // Zwei Auftraege sind durch den Interpreter gelaufen, also zwei Enden.
        assert_eq!(handle.stats.success.load(Ordering::Acquire), 2);
        assert_eq!(handle.stats.fail.load(Ordering::Acquire), 0);
    }
}
