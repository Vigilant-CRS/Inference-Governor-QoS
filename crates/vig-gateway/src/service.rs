//! Der gRPC-Dienst: die Uebersetzung zwischen Draht und Scheduling-Kern.
//!
//! ## Zwei Betriebsarten je Modell
//!
//! * **Konfiguriert.** Der Request laeuft durch den Scheduler: Frische,
//!   Deadline, Zulassung, Variantenwahl.
//! * **Unkonfiguriert.** Der Request wird unveraendert durchgereicht
//!   (Spec L-002). Das ist keine Notloesung, sondern die Integrationszusage:
//!   ein Kunde stellt den Endpunkt um und konfiguriert erst danach Modell fuer
//!   Modell die QoS-Regeln. Auch dort gilt das Nutzlastbudget.
//!
//! ## Warum die Methodenkoerper geboxt sind
//!
//! Die generierten OIP-Typen sind gross; ein Dienstfuture, das mehrere davon
//! als lokale Variablen haelt, wird schnell zweistellig kilobyteschwer. Der
//! Dienst hat 21 Methoden, und tonic fasst sie zu einem Future zusammen, das
//! so gross ist wie das groesste — ein Stack-Ueberlauf unter Last waere die
//! Folge. `Box::pin` legt den Koerper auf den Heap und laesst im aeusseren
//! Future nur einen Zeiger zurueck.
//!
//! ## Was dieser Dienst schuetzt (docs/security.md)
//!
//! * **Zugang** — [`Gate`], als Interceptor vor dem Dekodieren und hier noch
//!   einmal.
//! * **Administration** — Endpunkte, die den Zustand des Backends aendern,
//!   nur mit Administrationstoken (Security-Review H3).
//! * **Shared Memory** — Schluesselpraefix, Besitz, Obergrenze, und unter
//!   `trust: strict` nur eigene Regionen in Inferenzen, eigene Segmente und
//!   ein Status nur ueber eigene Regionen (H2, M4, N7; Review 15.09.2026
//!   R02, R07).

use crate::actor::{GraphRequest, Handle};
use crate::auth::{Gate, Identity, Tokens};
use crate::clock::MonotonicClock;
use crate::shm::{Refusal, Region, ShmRegistry, check_extent, check_key};
use vig_backend_triton::TritonClient;
use vig_config::schema::{Resolved, TrustMode};
use vig_core::{Criticality, Duration, PayloadRef, RequestDescriptor, RequestId, SupersessionKey};
use vig_protocol_oip::inference::grpc_inference_service_server::GrpcInferenceService;
// Der Dienst implementiert alle 21 Methoden des OIP-Dienstes; die Typen
// einzeln aufzuzaehlen waere eine Liste ohne Erkenntniswert, die bei jeder
// Protokollerweiterung nachgezogen werden muesste.
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tonic::{Request, Response, Status};
use vig_protocol_oip::inference::infer_parameter::ParameterChoice;
#[allow(clippy::wildcard_imports)]
use vig_protocol_oip::inference::*;
use vig_protocol_oip::params::{self, GenerationSource, HintRequest, VigParams};

/// Hoechstalter, ab dem ein Client-Zeitstempel als „aus einer anderen Uhr"
/// gilt, wenn das Modell kein `max_age` konfiguriert hat (ADR-0011).
const DEFAULT_PLAUSIBLE_AGE: Duration = Duration::from_nanos_unbounded(5_000_000_000);

/// Der Tensorparameter, mit dem OIP eine Shared-Memory-Region nennt.
pub const SHM_REGION_PARAM: &str = "shared_memory_region";

/// Wie oft hoechstens eine Warnung ueber unplausible Client-Zeitstempel
/// geschrieben wird (Security-Review N5).
///
/// Die Warnung beschreibt einen Integrationsfehler, keinen Einzelfall. Je
/// Request geschrieben, flutet ein fehlkonfigurierter oder boeswilliger
/// Client das Log; gezaehlt wird trotzdem jeder.
const GENERATION_WARNING_INTERVAL_NS: u64 = 10_000_000_000;

/// Der OIP-Dienst des Gateways.
#[derive(Debug)]
pub struct GatewayService {
    config: Arc<Resolved>,
    backend: Arc<TritonClient>,
    /// Der Client zum Server jedes konfigurierten Modells, in Indexreihenfolge.
    model_backends: Vec<Arc<TritonClient>>,
    scheduler: Handle,
    clock: MonotonicClock,
    next_id: AtomicU64,
    shm: ShmRegistry,
    budget: crate::budget::PayloadBudget,
    gate: Gate,
    /// Unplausible Client-Zeitstempel seit dem Start.
    generation_warnings: AtomicU64,
    /// Wann zuletzt darueber geschrieben wurde, in Nanosekunden der
    /// Dienstuhr; `0` heisst nie.
    last_generation_warning: AtomicU64,
}

/// Die Nutzlastgroesse eines Requests.
///
/// Bei Shared Memory reist der Tensor als Referenz, und der Request selbst ist
/// wenige hundert Bytes gross. Genau dann soll das Budget auch nichts
/// nennenswertes verbrauchen.
///
/// Gezaehlt werden **beide** zulaessigen Darstellungen. OIP erlaubt Rohdaten in
/// `raw_input_contents` und typisierte Werte in `inputs[].contents`; nur die
/// erste zu zaehlen hiess, dass ein Client das Budget umgeht, ohne etwas
/// Unerlaubtes zu tun — er benutzt schlicht die andere Form.
fn payload_bytes(request: &ModelInferRequest) -> u64 {
    let raw = request
        .raw_input_contents
        .iter()
        .map(|c| c.len() as u64)
        .fold(0_u64, u64::saturating_add);

    let typed = request
        .inputs
        .iter()
        .filter_map(|i| i.contents.as_ref())
        .map(tensor_content_bytes)
        .fold(0_u64, u64::saturating_add);

    raw.saturating_add(typed)
}

/// Die Groesse eines typisierten Tensorinhalts in Bytes.
///
/// Nicht die Zahl der Elemente, sondern ihr Speicher: ein `fp64`-Wert kostet
/// achtmal so viel wie ein `bool`. Wer Elemente zaehlt, deckelt einen
/// Doublevektor bei einem Achtel seines tatsaechlichen Bedarfs.
fn tensor_content_bytes(c: &InferTensorContents) -> u64 {
    let of = |count: usize, width: usize| (count as u64).saturating_mul(width as u64);
    of(c.bool_contents.len(), size_of::<bool>())
        .saturating_add(of(c.int_contents.len(), size_of::<i32>()))
        .saturating_add(of(c.int64_contents.len(), size_of::<i64>()))
        .saturating_add(of(c.uint_contents.len(), size_of::<u32>()))
        .saturating_add(of(c.uint64_contents.len(), size_of::<u64>()))
        .saturating_add(of(c.fp32_contents.len(), size_of::<f32>()))
        .saturating_add(of(c.fp64_contents.len(), size_of::<f64>()))
        .saturating_add(
            c.bytes_contents
                .iter()
                .map(|b| b.len() as u64)
                .fold(0_u64, u64::saturating_add),
        )
}

/// Die Shared-Memory-Regionen, die ein Request nennt — in Ein- und Ausgaben.
fn region_references(request: &ModelInferRequest) -> Vec<&str> {
    fn named(parameters: &std::collections::HashMap<String, InferParameter>) -> Option<&str> {
        match &parameters.get(SHM_REGION_PARAM)?.parameter_choice {
            Some(ParameterChoice::StringParam(name)) => Some(name.as_str()),
            _ => None,
        }
    }
    request
        .inputs
        .iter()
        .filter_map(|i| named(&i.parameters))
        .chain(request.outputs.iter().filter_map(|o| named(&o.parameters)))
        .collect()
}

/// Ob ein Hinweis mit dieser Geltungsdauer angenommen wird (N6).
fn hint_ttl_allowed(ttl: Duration, max: Option<Duration>) -> bool {
    max.is_none_or(|max| ttl <= max)
}

/// Die Antwort auf eine abgewiesene Registrierung oder Abmeldung.
fn shm_refusal(name: &str, refusal: Refusal) -> Status {
    match refusal {
        Refusal::Foreign => {
            Status::permission_denied(format!("die Region {name} gehoert einem anderen Aufrufer"))
        }
        // Wer das Segment haelt, bleibt ungesagt.
        Refusal::ForeignSegment => Status::permission_denied(
            "das Segment hinter diesem Schluessel ist bereits von einem anderen \
             Aufrufer registriert; im strikten Modus gehoert ein Segment genau \
             einem Aufrufer",
        ),
        Refusal::Unknown => Status::permission_denied(format!(
            "die Region {name} wurde nicht ueber diesen Governor \
             registriert; im strikten Modus meldet nur ihr Besitzer ab"
        )),
        Refusal::Busy => Status::aborted(format!(
            "fuer die Region {name} laeuft gerade eine Registrierung oder \
             Abmeldung; nach deren Antwort erneut versuchen"
        )),
        Refusal::Full { limit } => Status::resource_exhausted(format!(
            "hoechstens {limit} Regionen (backend.security.max_shm_regions)"
        )),
    }
}

/// Fuehrt einen reservierten Shared-Memory-Aufruf beim Backend aus und bucht
/// sein Ergebnis.
///
/// Der Aufruf laeuft als eigene Aufgabe. Bricht der Client ab, laeuft er
/// trotzdem zu Ende und wird gebucht oder zurueckgegeben: gaebe ein
/// Abbruch die Reservierung frei, obwohl das Backend schon registriert hat,
/// waere das Segment im Backend belegt und hier frei — fuer jeden anderen
/// Aufrufer (Review R02, R07). Scheitert der Aufruf, oder wird die Aufgabe
/// selbst abgebrochen, gibt die fallengelassene Reservierung ihren Platz
/// zurueck. Ein Backend, das nie antwortet, haelt sie so lange, wie seine
/// Verbindung besteht; das ist die vorsichtige Seite.
async fn settle_shm<T, F>(
    reservation: crate::shm::Reservation,
    call: F,
) -> Result<Response<T>, Status>
where
    T: Send + 'static,
    F: Future<Output = Result<Response<T>, Status>> + Send + 'static,
{
    let task = tokio::spawn(async move {
        let response = call.await?;
        reservation.confirm();
        Ok(response)
    });
    task.await
        .map_err(|e| Status::unavailable(format!("Shared-Memory-Aufruf abgebrochen: {e}")))?
}

impl GatewayService {
    /// Baut den Dienst.
    #[must_use]
    pub fn new(
        config: Arc<Resolved>,
        backend: Arc<TritonClient>,
        scheduler: Handle,
        clock: MonotonicClock,
    ) -> Self {
        let limit = config.max_inflight_bytes;
        let regions = config.security.max_shm_regions;
        // Ein Client je Endpunkt, nicht je Modell: Modelle desselben Servers
        // teilen sich den Kanal.
        let mut by_endpoint: Vec<(String, Arc<TritonClient>)> =
            vec![(config.backend_endpoint.clone(), Arc::clone(&backend))];
        let mut model_backends = Vec::with_capacity(config.model_names.len());
        for index in 0..config.model_names.len() {
            let endpoint = u16::try_from(index)
                .map_or(config.backend_endpoint.as_str(), |i| {
                    config.endpoint_of(vig_core::ModelIdx(i))
                })
                .to_owned();
            let client = if let Some((_, client)) = by_endpoint.iter().find(|(e, _)| *e == endpoint)
            {
                Arc::clone(client)
            } else {
                let client = Arc::new(TritonClient::new(&endpoint));
                by_endpoint.push((endpoint, Arc::clone(&client)));
                client
            };
            model_backends.push(client);
        }
        Self {
            config,
            backend,
            model_backends,
            scheduler,
            clock,
            next_id: AtomicU64::new(1),
            shm: ShmRegistry::with_limit(regions),
            budget: crate::budget::PayloadBudget::new(limit),
            gate: Gate::default(),
            generation_warnings: AtomicU64::new(0),
            last_generation_warning: AtomicU64::new(0),
        }
    }

    /// Schaltet die Bearer-Token-Pruefung ein.
    ///
    /// Ohne Aufruf ist sie aus. Bewusst als eigener Schritt und nicht aus der
    /// Konfiguration gelesen: der Dienst laedt keine Dateien; das tut der
    /// Aufrufer, und er kann einen Ladefehler melden, bevor irgendetwas
    /// lauscht.
    #[must_use]
    pub fn with_tokens(mut self, tokens: Tokens) -> Self {
        self.gate = self.gate.with_tokens(tokens);
        self
    }

    /// Hinterlegt die Administrationstoken (Security-Review H3).
    ///
    /// Ohne sie sind die Administrationsendpunkte gesperrt.
    #[must_use]
    pub fn with_admin_tokens(mut self, admin: Tokens) -> Self {
        self.gate = self.gate.with_admin(admin);
        self
    }

    /// Die Zugangspruefung, fuer den Interceptor vor dem Dekodieren.
    #[must_use]
    pub fn gate(&self) -> Gate {
        self.gate.clone()
    }

    /// Der Griff auf den Scheduler-Actor.
    ///
    /// Fuer Tests und Betriebswerkzeuge, die den Zustand abfragen wollen, ohne
    /// den Griff getrennt durchreichen zu muessen.
    #[must_use]
    pub fn scheduler_handle(&self) -> Handle {
        self.scheduler.clone()
    }

    /// Wie viele unplausible Client-Zeitstempel seit dem Start kamen.
    #[must_use]
    pub fn generation_warnings(&self) -> u64 {
        self.generation_warnings.load(Ordering::Relaxed)
    }

    /// Prueft die Zugangsberechtigung einer Anfrage.
    fn authorize<T>(&self, request: &Request<T>) -> Result<(), Status> {
        self.gate.admit(request)
    }

    /// Prueft, ob eine Anfrage einen Administrationsendpunkt nutzen darf.
    ///
    /// Die Endpunkte aendern den Zustand des Backends: Modelle laden und
    /// entladen, Tracing, Loglevel, CUDA-Speicher. Mit
    /// `--model-control-mode=explicit` entlaedt sonst jeder Inferenzclient das
    /// geschuetzte Modell — ein Denial of Service mit einem einzigen Aufruf.
    /// Deshalb gesperrt, bis ein Administrationstoken hinterlegt ist und
    /// vorgelegt wird, in jedem Vertrauensmodus.
    fn require_admin<T>(&self, request: &Request<T>) -> Result<(), Status> {
        self.authorize(request)?;
        if self.gate.is_admin(request) {
            Ok(())
        } else {
            Err(Status::permission_denied(
                "Administrationsendpunkt: nur mit Administrationstoken \
                 (backend.security.admin_token_file)",
            ))
        }
    }

    /// Die wirksame Wichtigkeitsklasse eines Requests.
    ///
    /// Im offenen Modus gilt die Clientangabe. Im strikten Modus darf sie die
    /// Klasse nur **senken**: sonst setzt sich jeder Aufrufer selbst auf
    /// `protected`, und die Prioritaeten sind eine Empfehlung statt einer
    /// Zusage. Ein Client, der freiwillig zurücktritt, ist dagegen harmlos —
    /// und nuetzlich.
    fn effective_class(
        &self,
        declared: Option<Criticality>,
        configured: Criticality,
    ) -> Criticality {
        match (self.config.trust, declared) {
            (TrustMode::Open, Some(class)) => class,
            (TrustMode::Strict, Some(class)) if class < configured => class,
            _ => configured,
        }
    }

    /// Der Bestand durchgereichter Shared-Memory-Regionen.
    #[must_use]
    pub const fn shm_registry(&self) -> &ShmRegistry {
        &self.shm
    }

    fn allocate_id(&self) -> RequestId {
        RequestId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    async fn raw(
        &self,
    ) -> Result<
        vig_protocol_oip::inference::grpc_inference_service_client::GrpcInferenceServiceClient<
            tonic::transport::Channel,
        >,
        Status,
    > {
        self.backend
            .raw()
            .await
            .map_err(|e| Status::unavailable(e.to_string()))
    }

    /// Der Kanal zum Backend eines konfigurierten Modells.
    ///
    /// Metadaten, Bereitschaft und Konfiguration eines Modells kennt nur der
    /// Server, der es rechnet — mit Ressourcendomaenen (NV-22) oder einem
    /// eigenen `backend_endpoint` ist das nicht `backend.grpc_endpoint`.
    /// Ohne Modell der Standardendpunkt, wie bisher.
    async fn raw_for(
        &self,
        model: Option<vig_core::ModelIdx>,
    ) -> Result<
        vig_protocol_oip::inference::grpc_inference_service_client::GrpcInferenceServiceClient<
            tonic::transport::Channel,
        >,
        Status,
    > {
        let client = model
            .and_then(|m| self.model_backends.get(m.get()))
            .unwrap_or(&self.backend);
        client
            .raw()
            .await
            .map_err(|e| Status::unavailable(e.to_string()))
    }

    /// Unter `trust: strict`: nennt der Request nur eigene Regionen?
    ///
    /// Eine Region, die ein anderer Aufrufer registriert hat — oder die
    /// niemand ueber diesen Governor registriert hat —, darf in keiner
    /// Inferenz stehen. Als Eingabe laesen die Frames eines anderen, als
    /// Ausgabe schriebe das Backend in fremden Speicher (Security-Review H2).
    fn check_region_references(
        &self,
        request: &ModelInferRequest,
        identity: Identity,
    ) -> Result<(), Status> {
        if self.config.trust != TrustMode::Strict {
            return Ok(());
        }
        for name in region_references(request) {
            if self.shm.owner_of(name) != Some(identity) {
                return Err(Status::permission_denied(format!(
                    "die Region {name} wurde nicht von diesem Aufrufer ueber den \
                     Governor registriert; im strikten Modus nennt eine Inferenz \
                     nur eigene Regionen"
                )));
            }
        }
        Ok(())
    }

    /// Reicht einen mitgegebenen Anwendungshinweis an den Kern durch (NV-18).
    ///
    /// Die Kennung kommt aus der Zugangsschicht und nicht aus dem Request:
    /// ADR-0029 verlangt, dass sie **belegt** und nicht behauptet wird. Ohne
    /// belegte Kennung — also ohne benanntes Token — passiert hier nichts.
    ///
    /// Ebenso ohne Geltungsdauer: ein Hinweis ohne Frist gaebe es nach
    /// ADR-0029 gar nicht, und einen Standardwert zu erfinden hiesse, eine
    /// Dauer zu setzen, die niemand vereinbart hat. Und nicht mit einer
    /// Frist ueber `hints.max_ttl_ms`: das waere eine dauerhafte
    /// Vertragsaenderung durch die Anwendung (Security-Review N6).
    fn offer_hint_from(
        &self,
        model: vig_core::ModelIdx,
        request: &ModelInferRequest,
        authority: Option<vig_core::hints::Authority>,
    ) {
        let Some(authority) = authority else {
            return;
        };
        let Ok(params): Result<VigParams, _> = params::extract(&request.parameters) else {
            return;
        };
        let (Some(requested), Some(ttl)) = (params.hint, params.hint_ttl) else {
            return;
        };
        if !hint_ttl_allowed(ttl, self.config.hint_max_ttl) {
            tracing::debug!(
                model = model.get(),
                ttl_ms = ttl.as_nanos().checked_div(1_000_000).unwrap_or(0),
                "Anwendungshinweis verworfen: Geltungsdauer ueber hints.max_ttl_ms"
            );
            return;
        }
        let kind = match requested {
            HintRequest::ActionHorizon(holds_for) => {
                vig_core::hints::HintKind::ActionHorizon { holds_for }
            }
            HintRequest::Elevated(max_age) => vig_core::hints::HintKind::Elevated { max_age },
            HintRequest::Mode(id) => vig_core::hints::HintKind::Mode { id },
        };
        self.scheduler.offer_hint(vig_core::hints::Hint {
            model,
            authority,
            kind,
            issued_at: self.clock.now(),
            ttl,
        });
    }

    /// Die Anmeldung eines Auftrags im Abhaengigkeitsgraphen (NV-17, ADR-0028).
    ///
    /// Die Kennung ist das `id`-Feld des OIP-Requests, als Zahl gelesen — die
    /// Groesse, die der Client ohnehin fuehrt und in `vig_depends_on` wieder
    /// nennt. Ohne `vig_capture_id` gibt es nichts anzumelden. Geprueft wird
    /// die Zusammenfuehrung im Actor, zusammen mit der Ankunft.
    ///
    /// # Errors
    ///
    /// `InvalidArgument`, wenn eine Zusammenfuehrung angemeldet wird, ohne
    /// dass der Request eine Aufnahme oder eine lesbare Kennung traegt — dann
    /// koennte niemand ihn spaeter als Elternteil nennen.
    fn graph_request(
        request: &ModelInferRequest,
        identity: Identity,
    ) -> Result<Option<GraphRequest>, Status> {
        let Ok(params): Result<VigParams, _> = params::extract(&request.parameters) else {
            return Ok(None);
        };
        let Some(capture) = params.capture_id else {
            if params.depends_on.is_empty() {
                return Ok(None);
            }
            return Err(Status::invalid_argument(
                "vig_depends_on ohne vig_capture_id: eine Zusammenfuehrung \
                 braucht die Aufnahme, zu der sie gehoert",
            ));
        };
        let Ok(id) = request.id.parse::<u64>() else {
            return Err(Status::invalid_argument(
                "vig_capture_id gesetzt, aber die Request-`id` ist keine Zahl; \
                 ohne sie kann kein spaeterer Auftrag diesen als Elternteil nennen",
            ));
        };
        Ok(Some(GraphRequest {
            owner: identity.0,
            capture: vig_core::dag::CaptureId(capture),
            id,
            parents: params.depends_on,
        }))
    }

    /// Meldet einen unplausiblen Client-Zeitstempel — gedrosselt (N5).
    ///
    /// Der haeufigste Integrationsfehler: der Parameter ist gesetzt, stammt
    /// aber aus einer anderen Zeitbasis. Ohne Meldung arbeitet Vigilant stumm
    /// ohne die Semantik, fuer die er da ist; mit einer Meldung je Request
    /// flutet ein einzelner Client das Log.
    fn warn_generation(&self, model: vig_core::ModelIdx, what: &'static str) {
        let total = self
            .generation_warnings
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let now = self.clock.now().as_nanos().max(1);
        let last = self.last_generation_warning.load(Ordering::Relaxed);
        let due = last == 0 || now.saturating_sub(last) >= GENERATION_WARNING_INTERVAL_NS;
        if due
            && self
                .last_generation_warning
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            tracing::warn!(
                model = model.get(),
                total,
                "{what} (hoechstens eine Meldung je 10 s; `total` zaehlt alle)"
            );
        }
    }

    /// Baut die Scheduling-Metadaten aus Request und Konfiguration.
    ///
    /// Was der Client angibt, gewinnt; was er weglaesst, kommt aus dem Vertrag
    /// (Spec 16.3). Ein fehlerhafter Parameter fuehrt zur Ablehnung, nicht zu
    /// einem stillen Default.
    fn build_descriptor(
        &self,
        model: vig_core::ModelIdx,
        request: &ModelInferRequest,
    ) -> Result<RequestDescriptor, Status> {
        let extracted: VigParams = params::extract(&request.parameters)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let contract = self
            .config
            .contracts
            .get(model.get())
            .ok_or_else(|| Status::internal("Modellindex ohne Vertrag"))?;

        let arrival = self.clock.now();
        let plausible = contract
            .max_age
            .and_then(|m| m.checked_scale(10, 1))
            .unwrap_or(DEFAULT_PLAUSIBLE_AGE);
        let (generation, source) = params::resolve_generation(&extracted, arrival, plausible);

        match source {
            GenerationSource::ArrivalFallback {
                rejected_client_value: true,
            } => self.warn_generation(
                model,
                "Generation-Time des Clients liegt in der Zukunft und wurde verworfen; \
                 Ankunftszeit wird verwendet",
            ),
            GenerationSource::ClampedAge => self.warn_generation(
                model,
                "Generation-Time des Clients ist unplausibel alt; auf die \
                 Plausibilitaetsgrenze geklemmt",
            ),
            GenerationSource::ArrivalFallback {
                rejected_client_value: false,
            }
            | GenerationSource::ClientAge
            | GenerationSource::ClientMonotonic => {}
        }

        let deadline = extracted.deadline.unwrap_or(contract.deadline);
        let absolute_deadline = generation.checked_add(deadline);
        if absolute_deadline.is_none() {
            return Err(Status::invalid_argument(
                "Generation Time plus Deadline ist nicht darstellbar",
            ));
        }

        Ok(RequestDescriptor {
            id: self.allocate_id(),
            logical_model: model,
            supersession_key: extracted
                .supersession_key
                .or(extracted.stream_id)
                .map_or(SupersessionKey::DEFAULT, SupersessionKey),
            generation_time: generation,
            arrival_time: arrival,
            absolute_deadline,
            max_age: extracted.max_age.or(contract.max_age),
            criticality: self.effective_class(extracted.class, contract.criticality),
            queue_policy: contract.queue.policy,
            stateful: contract.stateful,
            variant: None,
            payload: PayloadRef(0),
            context_tokens: 0,
            // Beides setzt der Actor, sobald feststeht, ob dieser Auftrag
            // wirklich zerlegt wird (NV-16).
            decomposable: false,
        })
    }

    fn budget_exhausted(&self, bytes: u64) -> Status {
        Status::resource_exhausted(format!(
            "Nutzlastbudget erschoepft: {bytes} Bytes angefordert, Obergrenze \
             {} Bytes",
            self.config.max_inflight_bytes
        ))
    }

    async fn infer(
        &self,
        request: Request<ModelInferRequest>,
    ) -> Result<ModelInferResponse, Status> {
        // Vor allem anderen: unautorisierte Arbeit soll nicht einmal die
        // Parameter kosten, die ihr Auslesen braucht.
        self.authorize(&request)?;
        // Identitaet und Hinweis-Kennung, bevor der Request verbraucht wird.
        let identity = self.gate.identity_of(&request);
        let authority = self.gate.authority_of(&request);
        let inner = request.into_inner();
        self.check_region_references(&inner, identity)?;

        // Bytebudget: der Ereigniskanal begrenzt die Anzahl offener Requests,
        // nicht ihren Speicher. Ohne diese Schranke haelt ein Aufrufer mit
        // grossen Tensoren den Prozess an die Speicherwand, lange bevor die
        // Requestzahl auffaellt.
        let bytes = payload_bytes(&inner);

        let Some(model) = self.config.model_index(&inner.model_name) else {
            // Unkonfiguriertes Modell: unveraendert durchreichen (Spec L-002).
            //
            // Im strikten Modus nicht: sonst genuegt der **physische**
            // Modellname, um die gesamte Steuerung zu umgehen. Wer
            // `detector_large` statt `detector` aufruft, laeuft dann ohne
            // Kredit, ohne Frischepruefung und ohne Look-ahead — und
            // verdraengt genau die geschuetzte Arbeit, die der Governor
            // schuetzen soll.
            if self.config.trust == TrustMode::Strict {
                return Err(Status::not_found(format!(
                    "Modell {} ist nicht konfiguriert; im strikten Modus wird nicht \
                     am Governor vorbei ausgefuehrt",
                    inner.model_name
                )));
            }
            // Auch durchgereicht zaehlt die Nutzlast gegen das Budget, und
            // zwar bis die Antwort da ist (Security-Review M2). Sonst hebelt
            // ein grosser Request an ein unkonfiguriertes Modell die
            // Speichergrenze aus.
            let Some(permit) = self.budget.try_reserve(bytes) else {
                return Err(self.budget_exhausted(bytes));
            };
            let result = self.raw().await?.model_infer(inner).await;
            drop(permit);
            return result.map(Response::into_inner);
        };

        let Some(permit) = self.budget.try_reserve(bytes) else {
            return Err(self.budget_exhausted(bytes));
        };

        // NV-18: ein mitgegebener Hinweis geht an den Kern, bevor der Request
        // eingereiht wird — er soll fuer diesen Request schon gelten. Er wird
        // **nicht** beantwortet: was die Policy des Betreibers damit macht,
        // ist eine Betriebsfrage, und ein abgelehnter Hinweis darf den
        // Request, der ihn mitgebracht hat, nicht scheitern lassen.
        self.offer_hint_from(model, &inner, authority);

        // NV-17: nennt der Client eine Aufnahme, meldet sich der Auftrag mit
        // seiner Ankunft im Graphen an — und eine Zusammenfuehrung ueber
        // Aufnahmegrenzen wird abgelehnt, **bevor** sie rechnet. Ohne
        // `vig_capture_id` passiert nichts, und der Governor plant nach
        // Frische allein.
        let graph = Self::graph_request(&inner, identity)?;

        let descriptor = self.build_descriptor(model, &inner)?;
        let logical = inner.model_name.clone();
        // Der Guard reist mit: das Budget endet mit der Ausfuehrung, nicht mit
        // diesem Aufruf.
        let mut response = self
            .scheduler
            .submit_with_graph(descriptor, inner, permit, graph)
            .await?;
        // Nach aussen existiert nur das logische Modell. Welche Variante
        // gelaufen ist, ist eine interne Entscheidung — steht ihr Name in der
        // Antwort, koppelt sich der Client daran, und die Variantenwahl waere
        // faktisch nicht mehr frei. `model_metadata` haelt es genauso.
        response.model_name = logical;
        Ok(response)
    }
}

#[tonic::async_trait]
impl GrpcInferenceService for GatewayService {
    async fn model_infer(
        &self,
        request: Request<ModelInferRequest>,
    ) -> Result<Response<ModelInferResponse>, Status> {
        Box::pin(self.infer(request)).await.map(Response::new)
    }

    async fn model_ready(
        &self,
        request: Request<ModelReadyRequest>,
    ) -> Result<Response<ModelReadyResponse>, Status> {
        self.authorize(&request)?;
        let mut inner = request.into_inner();
        let model = self.config.model_index(&inner.name);
        if let Some(physical) = model.and_then(|m| self.config.backend_model(m, 0)) {
            // Ein logisches Modell ist bereit, wenn seine beste Variante es ist.
            inner.name = physical.to_owned();
        }
        self.raw_for(model).await?.model_ready(inner).await
    }

    async fn model_metadata(
        &self,
        request: Request<ModelMetadataRequest>,
    ) -> Result<Response<ModelMetadataResponse>, Status> {
        self.authorize(&request)?;
        let mut inner = request.into_inner();
        let logical = inner.name.clone();
        let model = self.config.model_index(&logical);
        let mapped = model.and_then(|m| self.config.backend_model(m, 0).map(ToOwned::to_owned));
        if let Some(physical) = mapped {
            inner.name = physical;
        }
        let mut response = self
            .raw_for(model)
            .await?
            .model_metadata(inner)
            .await?
            .into_inner();
        // Der Client hat nach dem logischen Modell gefragt und bekommt es auch
        // zurueck; welche Variante dahinterliegt, ist seine Sache nicht.
        response.name = logical;
        Ok(Response::new(response))
    }

    /// Lebt der Governor? Lokal beantwortet (Security-Review N3).
    ///
    /// Frueher fragte jeder Aufruf das Backend — ungeprueft, und damit ein
    /// Verstaerker: jeder im Netz konnte Tritons Health-Endpunkt ueber den
    /// Governor im Takt aufrufen. Ob der Governor lebt, weiss er selbst.
    async fn server_live(
        &self,
        request: Request<ServerLiveRequest>,
    ) -> Result<Response<ServerLiveResponse>, Status> {
        self.authorize(&request)?;
        Ok(Response::new(ServerLiveResponse { live: true }))
    }

    /// Kann der Governor etwas ausrichten? Aus seinem eigenen Zustand
    /// beantwortet: aktive Erreichbarkeitsprobe je Backend und Quarantaene —
    /// dieselbe Entscheidung wie `/readyz`.
    async fn server_ready(
        &self,
        request: Request<ServerReadyRequest>,
    ) -> Result<Response<ServerReadyResponse>, Status> {
        self.authorize(&request)?;
        let ready = crate::exporter::ready(&self.scheduler).await.is_ok();
        Ok(Response::new(ServerReadyResponse { ready }))
    }

    async fn server_metadata(
        &self,
        request: Request<ServerMetadataRequest>,
    ) -> Result<Response<ServerMetadataResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .server_metadata(request.into_inner())
            .await
    }

    async fn model_config(
        &self,
        request: Request<ModelConfigRequest>,
    ) -> Result<Response<ModelConfigResponse>, Status> {
        self.authorize(&request)?;
        let inner = request.into_inner();
        let model = self.config.model_index(&inner.name);
        Box::pin(async move { self.raw_for(model).await?.model_config(inner).await }).await
    }

    async fn model_statistics(
        &self,
        request: Request<ModelStatisticsRequest>,
    ) -> Result<Response<ModelStatisticsResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .model_statistics(request.into_inner())
            .await
    }

    async fn repository_index(
        &self,
        request: Request<RepositoryIndexRequest>,
    ) -> Result<Response<RepositoryIndexResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .repository_index(request.into_inner())
            .await
    }

    async fn repository_model_load(
        &self,
        request: Request<RepositoryModelLoadRequest>,
    ) -> Result<Response<RepositoryModelLoadResponse>, Status> {
        self.require_admin(&request)?;
        self.raw()
            .await?
            .repository_model_load(request.into_inner())
            .await
    }

    async fn repository_model_unload(
        &self,
        request: Request<RepositoryModelUnloadRequest>,
    ) -> Result<Response<RepositoryModelUnloadResponse>, Status> {
        self.require_admin(&request)?;
        self.raw()
            .await?
            .repository_model_unload(request.into_inner())
            .await
    }

    // Shared-Memory-Endpunkte werden durchgereicht; der Governor beruehrt die
    // Payload dabei nie (ADR-0003). Registrierung und Abmeldung sind aber
    // geprueft: Schluesselpraefix, Ausdehnung, Besitz von Name und Segment,
    // Obergrenze.
    async fn system_shared_memory_status(
        &self,
        request: Request<SystemSharedMemoryStatusRequest>,
    ) -> Result<Response<SystemSharedMemoryStatusResponse>, Status> {
        self.authorize(&request)?;
        let identity = self.gate.identity_of(&request);
        // Im strikten Modus sieht ein Aufrufer nur seine eigenen Regionen:
        // Namen und Schluessel fremder Regionen sind genau die Angaben, mit
        // denen er sich an ein fremdes Segment haengen wollte (Review R02).
        // Eine fremde und eine unbekannte Region sind nicht unterscheidbar.
        let confined = self.config.trust == TrustMode::Strict && !self.gate.is_admin(&request);
        Box::pin(async move {
            let inner = request.into_inner();
            if confined
                && !inner.name.is_empty()
                && self.shm.owner_of(&inner.name) != Some(identity)
            {
                return Err(Status::permission_denied(format!(
                    "die Region {} wurde nicht von diesem Aufrufer ueber den \
                     Governor registriert; im strikten Modus zeigt der Status nur \
                     eigene Regionen",
                    inner.name
                )));
            }
            let mut response = self.raw().await?.system_shared_memory_status(inner).await?;
            if confined {
                response
                    .get_mut()
                    .regions
                    .retain(|name, _| self.shm.owner_of(name) == Some(identity));
            }
            Ok(response)
        })
        .await
    }

    async fn system_shared_memory_register(
        &self,
        request: Request<SystemSharedMemoryRegisterRequest>,
    ) -> Result<Response<SystemSharedMemoryRegisterResponse>, Status> {
        self.authorize(&request)?;
        let identity = self.gate.identity_of(&request);
        Box::pin(async move {
            let inner = request.into_inner();
            if inner.name.is_empty() {
                return Err(Status::invalid_argument("eine Region braucht einen Namen"));
            }
            check_key(&self.config.security.shm_key_prefix, &inner.key)
                .map_err(Status::invalid_argument)?;
            check_extent(inner.offset, inner.byte_size).map_err(Status::invalid_argument)?;
            let region = Region {
                key: inner.key.clone(),
                byte_size: inner.byte_size,
                offset: inner.offset,
                cuda: false,
                owner: identity,
            };
            // Reservieren vor dem Backendaufruf, buchen erst nach dessen
            // Bestaetigung: sonst saehen parallele Registrierungen denselben
            // freien Platz (Review R07), und Vigilant fuehrte Regionen, die es
            // im Backend gar nicht gibt.
            let strict = self.config.trust == TrustMode::Strict;
            let reservation = self
                .shm
                .reserve_registration(&inner.name, region, strict)
                .map_err(|refusal| shm_refusal(&inner.name, refusal))?;
            let mut client = self.raw().await?;
            settle_shm(reservation, async move {
                client.system_shared_memory_register(inner).await
            })
            .await
        })
        .await
    }

    async fn system_shared_memory_unregister(
        &self,
        request: Request<SystemSharedMemoryUnregisterRequest>,
    ) -> Result<Response<SystemSharedMemoryUnregisterResponse>, Status> {
        self.authorize(&request)?;
        let identity = self.gate.identity_of(&request);
        let admin = self.gate.is_admin(&request);
        Box::pin(async move {
            let inner = request.into_inner();
            // Ein leerer Name heisst im Protokoll „alle Regionen" — auch die
            // aller anderen Clients (Security-Review M4).
            if inner.name.is_empty() && !admin {
                return Err(Status::permission_denied(
                    "alle Regionen abmelden nur mit Administrationstoken",
                ));
            }
            let strict = self.config.trust == TrustMode::Strict;
            let reservation = self
                .shm
                .reserve_unregistration(&inner.name, (!admin).then_some(identity), strict)
                .map_err(|refusal| shm_refusal(&inner.name, refusal))?;
            let mut client = self.raw().await?;
            settle_shm(reservation, async move {
                client.system_shared_memory_unregister(inner).await
            })
            .await
        })
        .await
    }

    async fn cuda_shared_memory_status(
        &self,
        request: Request<CudaSharedMemoryStatusRequest>,
    ) -> Result<Response<CudaSharedMemoryStatusResponse>, Status> {
        self.authorize(&request)?;
        self.raw()
            .await?
            .cuda_shared_memory_status(request.into_inner())
            .await
    }

    // CUDA-Speicher wird ueber Geraetehandles registriert, die der Governor
    // weder pruefen noch einem Besitzer zuordnen kann. Deshalb Administration.
    async fn cuda_shared_memory_register(
        &self,
        request: Request<CudaSharedMemoryRegisterRequest>,
    ) -> Result<Response<CudaSharedMemoryRegisterResponse>, Status> {
        self.require_admin(&request)?;
        self.raw()
            .await?
            .cuda_shared_memory_register(request.into_inner())
            .await
    }

    async fn cuda_shared_memory_unregister(
        &self,
        request: Request<CudaSharedMemoryUnregisterRequest>,
    ) -> Result<Response<CudaSharedMemoryUnregisterResponse>, Status> {
        self.require_admin(&request)?;
        self.raw()
            .await?
            .cuda_shared_memory_unregister(request.into_inner())
            .await
    }

    async fn trace_setting(
        &self,
        request: Request<TraceSettingRequest>,
    ) -> Result<Response<TraceSettingResponse>, Status> {
        self.require_admin(&request)?;
        Box::pin(async move { self.raw().await?.trace_setting(request.into_inner()).await }).await
    }

    async fn log_settings(
        &self,
        request: Request<LogSettingsRequest>,
    ) -> Result<Response<LogSettingsResponse>, Status> {
        self.require_admin(&request)?;
        Box::pin(async move { self.raw().await?.log_settings(request.into_inner()).await }).await
    }

    type ModelStreamInferStream = tokio_stream_placeholder::Never;

    /// Streaming-Inferenz ist im MVP nicht unterstuetzt (Spec 16.1).
    ///
    /// Bewusst ein klarer Fehler statt eines stillen Durchreichens: ein
    /// gestreamter Request wuerde die Frische- und Zulassungslogik umgehen und
    /// dem Nutzer vorspiegeln, Vigilant wirke, wo er nicht wirkt.
    async fn model_stream_infer(
        &self,
        request: Request<tonic::Streaming<ModelInferRequest>>,
    ) -> Result<Response<Self::ModelStreamInferStream>, Status> {
        self.authorize(&request)?;
        Err(Status::unimplemented(
            "ModelStreamInfer wird von Vigilant nicht unterstuetzt; \
             fuer gestreamte Inferenz direkt gegen das Backend arbeiten",
        ))
    }
}

/// Ein Stream, der nie etwas liefert.
///
/// Platzhalter fuer den nicht unterstuetzten Streaming-Endpunkt: der
/// zugehoerige Aufruf endet immer mit `Unimplemented`, der Typ wird trotzdem
/// gebraucht.
mod tokio_stream_placeholder {
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tonic::codegen::tokio_stream::Stream;
    use vig_protocol_oip::inference::ModelStreamInferResponse;

    /// Ein leerer Stream.
    #[derive(Debug)]
    pub struct Never;

    impl Stream for Never {
        type Item = Result<ModelStreamInferResponse, tonic::Status>;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(None)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v).unwrap()
    }

    /// N6: ein Hinweis mit einer Frist ueber der Obergrenze wird verworfen.
    #[test]
    fn a_hint_longer_than_the_limit_is_dropped() {
        assert!(hint_ttl_allowed(ms(500), Some(ms(60_000))));
        assert!(hint_ttl_allowed(ms(60_000), Some(ms(60_000))));
        assert!(!hint_ttl_allowed(ms(60_001), Some(ms(60_000))));
        // Ohne Hinweisblock gibt es auch keine Obergrenze — und keine Hinweise.
        // Die laengste Spanne, die es ueberhaupt gibt: eine Stunde.
        assert!(hint_ttl_allowed(Duration::MAX_CONTRACT, None));
    }

    /// H2: Regionen stehen in Ein- **und** Ausgaben.
    #[test]
    fn region_references_are_found_in_inputs_and_outputs() {
        let region = |name: &str| {
            let mut p = std::collections::HashMap::new();
            p.insert(
                SHM_REGION_PARAM.to_owned(),
                InferParameter {
                    parameter_choice: Some(ParameterChoice::StringParam(name.to_owned())),
                },
            );
            p
        };
        let request = ModelInferRequest {
            inputs: vec![model_infer_request::InferInputTensor {
                parameters: region("in"),
                ..Default::default()
            }],
            outputs: vec![model_infer_request::InferRequestedOutputTensor {
                parameters: region("out"),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(region_references(&request), vec!["in", "out"]);
    }
}
