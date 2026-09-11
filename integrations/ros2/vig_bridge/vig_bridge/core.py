"""Die Logik der Bruecke, ohne ROS und ohne gRPC.

Alles, was hier steht, laesst sich ohne laufendes ROS pruefen. Die Knoten-
und Transportschicht (`node.py`, `oip.py`) verdrahtet nur, was hier
entschieden wird.

Die Uhrenfrage
--------------
Der Governor rechnet mit seiner eigenen monotonen Uhr. Ein ROS-Zeitstempel
(`header.stamp`) stammt aus einer anderen Zeitbasis: der ROS-Uhr des
Knotens, der das Bild aufgenommen hat — Systemzeit, oder bei
`use_sim_time` die simulierte Zeit. Ein absoluter Zeitstempel ueber diese
Grenze hinweg ist bedeutungslos.

Die Bruecke schickt deshalb **kein** absolutes Datum, sondern ein **Alter**
(`vig_age_us`): `jetzt - header.stamp`, beides in der ROS-Uhr desselben
Knotens gemessen, im Moment des Absendens. Das Alter ist ueber Uhrgrenzen
hinweg gueltig, solange zwei Bedingungen gelten:

* Die Kamera und die Bruecke teilen eine ROS-Uhr, oder ihre Uhren sind
  synchronisiert (PTP/chrony). Laeuft der Kameratreiber auf einem anderen
  Rechner ohne Synchronisation, ist das Alter um den Versatz falsch.
* Die ROS-Uhr laeuft mit Echtzeit. Bei `use_sim_time` gilt das nicht: eine
  Simulation, die langsamer, schneller oder gar nicht laeuft, erzeugt Alter
  in Simulationssekunden, und der Governor misst Deadlines in echten. Mit
  `age_source: arrival` schickt die Bruecke dann gar kein Alter — der
  Governor zaehlt ab Ankunft, und das ist ehrlich falsch statt verdeckt.

Ein **negatives** Alter (Stempel in der Zukunft) ist ein Uhrenfehler. Bis zu
`max_clock_skew_us` wird es auf null geklemmt, darueber zusaetzlich als
Uhrenversatz gemeldet. Ein negatives Alter still zu verrechnen, hiesse, dem
Governor einen frischeren Frame zu versprechen, als es ihn gibt.
"""

from __future__ import annotations

import dataclasses
import enum
import itertools
import os
import threading
from collections import deque
from typing import Deque, Dict, Iterable, List, Optional, Sequence, Tuple

# --------------------------------------------------------------------------
# Alter
# --------------------------------------------------------------------------


@dataclasses.dataclass(frozen=True)
class Age:
    """Das Alter eines Frames beim Absenden."""

    micros: int
    """Das Alter in Mikrosekunden, nie negativ."""
    clock_skew: bool
    """Der Stempel lag weiter in der Zukunft als erlaubt."""


def age_of(stamp_ns: int, now_ns: int, max_clock_skew_us: int = 10_000) -> Age:
    """Das Alter eines Frames aus seinem Stempel und der aktuellen Uhrzeit.

    Beide Werte muessen aus derselben ROS-Uhr stammen. Ein Stempel in der
    Zukunft wird auf null geklemmt; liegt er mehr als `max_clock_skew_us`
    voraus, wird das als Uhrenversatz gemeldet.
    """
    delta_ns = now_ns - stamp_ns
    if delta_ns >= 0:
        return Age(micros=delta_ns // 1_000, clock_skew=False)
    ahead_us = (-delta_ns) // 1_000
    return Age(micros=0, clock_skew=ahead_us > max_clock_skew_us)


# --------------------------------------------------------------------------
# Kennungen
# --------------------------------------------------------------------------


class RequestIds:
    """Numerische Request-Kennungen, eindeutig ueber Prozesse hinweg.

    Der Governor verlangt eine **numerische** `id`, sobald `vig_capture_id`
    gesetzt ist — sonst koennte kein spaeterer Auftrag diesen als
    Elternteil nennen (NV-17). Mehrere Bruecken an einem Governor duerfen
    sich dabei nicht in die Quere kommen: die oberen Bits tragen einen
    Prozessanteil, die unteren einen Zaehler. Das Ergebnis bleibt unter
    2^63 und passt damit in jedes int64.
    """

    def __init__(self, process_tag: Optional[int] = None) -> None:
        tag = os.getpid() if process_tag is None else process_tag
        self._base = (tag & 0x7FFF_FFFF) << 32
        self._counter = itertools.count(1)
        self._lock = threading.Lock()

    def next(self) -> int:
        with self._lock:
            return self._base | (next(self._counter) & 0xFFFF_FFFF)


class CaptureIds:
    """Ordnet Frames derselben Aufnahme dieselbe `vig_capture_id` zu (NV-17).

    Zwei Kameras einer Aufnahmegruppe, die mit demselben (oder fast demselben)
    Stempel ausgeloest wurden, gehoeren zu einer Aufnahme. Welche Topics eine
    Gruppe bilden, sagt die Konfiguration — die Bruecke erraet das nicht.

    Begrenzt: je Gruppe werden hoechstens `memory` juengste Aufnahmen
    gemerkt. Ein Frame, der spaeter kommt als das, gilt als neue Aufnahme.
    """

    def __init__(self, tolerance_ns: int = 1_000_000, memory: int = 64) -> None:
        self._tolerance_ns = tolerance_ns
        self._memory = memory
        self._recent: Dict[str, Deque[Tuple[int, int]]] = {}
        self._next = itertools.count(1)
        self._lock = threading.Lock()

    def assign(self, group: str, stamp_ns: int) -> int:
        with self._lock:
            recent = self._recent.setdefault(group, deque(maxlen=self._memory))
            for known_stamp, capture in recent:
                if abs(known_stamp - stamp_ns) <= self._tolerance_ns:
                    return capture
            capture = next(self._next)
            recent.append((stamp_ns, capture))
            return capture


# --------------------------------------------------------------------------
# Parameter
# --------------------------------------------------------------------------


@dataclasses.dataclass(frozen=True)
class FreshnessParams:
    """Die `vig_`-Parameter eines Requests (docs/getting-started.md)."""

    age_us: Optional[int]
    capture_id: Optional[int] = None
    supersession_key: Optional[int] = None
    max_age_us: Optional[int] = None
    deadline_us: Optional[int] = None

    def as_dict(self) -> Dict[str, int]:
        """Nur die gesetzten Werte, jeweils als int64.

        Ein Parameter mit dem Wert `None` wird weggelassen, nicht als null
        geschickt: der Governor liest einen fehlenden Parameter als
        „Vertrag", eine Null als Zusage.
        """
        out: Dict[str, int] = {}
        for name, value in (
            ("vig_age_us", self.age_us),
            ("vig_capture_id", self.capture_id),
            ("vig_supersession_key", self.supersession_key),
            ("vig_max_age_us", self.max_age_us),
            ("vig_deadline_us", self.deadline_us),
        ):
            if value is None:
                continue
            if value < 0 or value >= 2**63:
                raise ValueError(f"{name}={value} liegt ausserhalb von int64")
            out[name] = int(value)
        return out


# --------------------------------------------------------------------------
# Ablehnungen
# --------------------------------------------------------------------------


class Outcome(enum.Enum):
    """Wie ein Request endete, aus Sicht der Anwendung."""

    DELIVERED = "delivered"
    DELIVERED_OBSOLETE = "delivered_obsolete"
    SUPERSEDED = "superseded"
    STALE = "stale"
    INFEASIBLE = "infeasible"
    CAPACITY = "capacity"
    BACKEND_FAILED = "backend_failed"
    EXECUTION_UNKNOWN = "execution_unknown"
    BACKEND_TIMEOUT = "backend_timeout"
    CANCELLED = "cancelled"
    FUSION_REFUSED = "fusion_refused"
    INVALID = "invalid"
    NOT_FOUND = "not_found"
    TRANSPORT = "transport"
    UNKNOWN = "unknown"

    @property
    def is_refusal(self) -> bool:
        """Eine Entscheidung des Governors, kein Fehler.

        Ein ueberholter oder zu alter Frame ist genau das Produkt bei der
        Arbeit. Er gehoert in die Diagnose, nicht ins Fehlerprotokoll.
        """
        return self in {
            Outcome.SUPERSEDED,
            Outcome.STALE,
            Outcome.INFEASIBLE,
            Outcome.CAPACITY,
            Outcome.FUSION_REFUSED,
        }

    @property
    def ends_reading(self) -> bool:
        """Ist sicher, dass niemand den Eingabepuffer dieses Requests mehr liest?

        Ja, wenn das Backend geantwortet hat (auch mit einem Fehler) oder
        der Governor den Request abgewiesen hat, bevor er ihn weitergab.
        Nein bei einem Timeout, einem unbekannten Ausgang, einem Abbruch oder
        einem Transportfehler: das Backend kann dann noch rechnen und aus dem
        Fach lesen, und der Governor haelt aus genau diesem Grund seinen
        Ausfuehrungskredit fest.
        """
        return self in {
            Outcome.DELIVERED,
            Outcome.DELIVERED_OBSOLETE,
            Outcome.SUPERSEDED,
            Outcome.STALE,
            Outcome.INFEASIBLE,
            Outcome.CAPACITY,
            Outcome.FUSION_REFUSED,
            Outcome.INVALID,
            Outcome.NOT_FOUND,
            Outcome.BACKEND_FAILED,
        }

    @property
    def retry_same_frame(self) -> bool:
        """Lohnt es, **denselben** Frame noch einmal zu schicken?

        Fast nie: der naechste Frame ist frischer. Nur ein Transportfehler
        ohne Antwort des Governors laesst offen, ob er ueberhaupt ankam.
        """
        return self is Outcome.TRANSPORT


_REASONS = {
    "superseded": Outcome.SUPERSEDED,
    "stale": Outcome.STALE,
    "infeasible": Outcome.INFEASIBLE,
    # Gruende des Aufnahmegraphen (NV-17). Heute schickt der Governor bei
    # einer abgelehnten Anmeldung nur FAILED_PRECONDITION ohne Grund; sobald
    # er sie unterscheidet, soll „Graph voll" nicht als Fusionsfehler der
    # Anwendung erscheinen.
    "capture_mismatch": Outcome.FUSION_REFUSED,
    "graph_full": Outcome.CAPACITY,
    "backend_failed": Outcome.BACKEND_FAILED,
    "execution_unknown": Outcome.EXECUTION_UNKNOWN,
    "backend_timeout": Outcome.BACKEND_TIMEOUT,
    "cancelled": Outcome.CANCELLED,
}


def classify_error(code: str, reason: Optional[str], message: str = "") -> Outcome:
    """Ordnet eine gRPC-Fehlerantwort des Governors ein.

    `code` ist der Name des gRPC-Codes (`"ABORTED"`, …), `reason` der Wert
    des Trailing-Metadatums `vig-reason`, falls vorhanden. Der Grund gewinnt
    ueber den Code: `ABORTED` kann ueberholt oder veraltet heissen, und die
    Anwendung soll das unterscheiden koennen, ohne Fehlertexte zu parsen.
    """
    if reason and reason in _REASONS:
        return _REASONS[reason]
    code = code.upper().removeprefix("STATUSCODE.")
    if code == "RESOURCE_EXHAUSTED":
        # Ohne Grund: der Governor nimmt gerade gar nichts an (Backpressure
        # vor dem Scheduler), nicht „dieser Request passt nicht mehr".
        return Outcome.CAPACITY
    if code == "FAILED_PRECONDITION":
        return Outcome.FUSION_REFUSED
    if code == "INVALID_ARGUMENT":
        return Outcome.INVALID
    if code == "NOT_FOUND":
        return Outcome.NOT_FOUND
    if code in ("UNAVAILABLE", "DEADLINE_EXCEEDED") and not reason:
        return Outcome.TRANSPORT
    if code == "CANCELLED":
        return Outcome.CANCELLED
    return Outcome.UNKNOWN


# --------------------------------------------------------------------------
# Bild -> Tensor
# --------------------------------------------------------------------------

_CHANNELS = {"rgb8": 3, "bgr8": 3, "rgba8": 4, "bgra8": 4, "mono8": 1}


@dataclasses.dataclass(frozen=True)
class TensorSpec:
    """Die Eingabe eines Modells, wie der Governor sie meldet."""

    name: str
    datatype: str
    shape: Tuple[int, ...]
    layout: str = "NCHW"

    @property
    def batch(self) -> int:
        return self.shape[0] if len(self.shape) == 4 else 1

    @property
    def height(self) -> int:
        return self.shape[2] if self.layout == "NCHW" else self.shape[1]

    @property
    def width(self) -> int:
        return self.shape[3] if self.layout == "NCHW" else self.shape[2]

    @property
    def channels(self) -> int:
        return self.shape[1] if self.layout == "NCHW" else self.shape[3]

    @property
    def byte_size(self) -> int:
        size = {"FP32": 4, "FP16": 2, "UINT8": 1, "INT8": 1}.get(self.datatype, 4)
        total = size
        for dim in self.shape:
            total *= dim
        return total


def image_to_tensor(
    data: bytes,
    width: int,
    height: int,
    encoding: str,
    step: int,
    spec: TensorSpec,
    scale: float = 1.0 / 255.0,
    fill_batch: bool = False,
):
    """Wandelt ein `sensor_msgs/Image` in den Eingabetensor des Modells.

    Groesse per naechstem Nachbarn — bewusst ohne OpenCV: die Bruecke soll
    auf jedem ROS-Rechner laufen, und die Vorverarbeitung eines echten
    Modells gehoert ohnehin in dessen Pipeline, nicht in eine generische
    Bruecke. `fill_batch` wiederholt den Frame, wenn das Modell eine feste
    Batchgroesse groesser eins erwartet; ohne es wird so ein Modell
    abgelehnt, statt still mit Nullen gefuellt.
    """
    import numpy as np

    channels = _CHANNELS.get(encoding)
    if channels is None:
        raise ValueError(f"Bildkodierung {encoding!r} wird nicht unterstuetzt")
    rows = np.frombuffer(data, dtype=np.uint8)
    if step * height > rows.size:
        raise ValueError("Bilddaten kuerzer als step * height")
    image = rows[: step * height].reshape(height, step)[:, : width * channels]
    image = image.reshape(height, width, channels)

    if encoding in ("bgr8", "bgra8"):
        image = image[:, :, [2, 1, 0] + ([3] if channels == 4 else [])]
    if spec.channels == 3 and channels == 4:
        image = image[:, :, :3]
    elif spec.channels == 3 and channels == 1:
        image = np.repeat(image, 3, axis=2)
    elif spec.channels != image.shape[2]:
        raise ValueError(
            f"Modell erwartet {spec.channels} Kanaele, Bild hat {image.shape[2]}"
        )

    ys = (np.arange(spec.height) * height // spec.height).clip(0, height - 1)
    xs = (np.arange(spec.width) * width // spec.width).clip(0, width - 1)
    resized = image[ys][:, xs]

    if spec.datatype == "UINT8":
        tensor = resized.astype(np.uint8)
    else:
        dtype = np.float16 if spec.datatype == "FP16" else np.float32
        tensor = resized.astype(dtype) * dtype(scale)
    if spec.layout == "NCHW":
        tensor = tensor.transpose(2, 0, 1)
    tensor = tensor[np.newaxis, ...]

    if spec.batch > 1:
        if not fill_batch:
            raise ValueError(
                f"Modell erwartet feste Batchgroesse {spec.batch}; "
                "fill_batch setzen oder das Modell anpassen"
            )
        tensor = np.repeat(tensor, spec.batch, axis=0)
    return np.ascontiguousarray(tensor)


# --------------------------------------------------------------------------
# Shared-Memory-Ring
# --------------------------------------------------------------------------


class SlotRing:
    """Vergibt Shared-Memory-Faecher, die gerade kein Request mehr liest.

    Mit **einem** Fach je Kamera wuerde ein neuer Frame die Region
    ueberschreiben, waehrend der vorige noch im Governor wartet oder im
    Backend gelesen wird — ein zerrissener Frame, den niemand bemerkt. Ein
    Fach wird deshalb erst wieder vergeben, wenn der Request, der es
    benutzt, beantwortet ist. Sind alle belegt, ist das Backpressure: der
    neue Frame wird nicht geschickt und gezaehlt.

    "Beantwortet" allein reicht nicht (Review R04): nach einem Timeout oder
    einem unbekannten Ausgang kann das Backend das Fach noch lesen. Ein
    solches Fach kommt in **Quarantaene** und wird nie wieder vergeben. Ein
    Timer waere kein Nachweis, dass der Leser fertig ist. Sind alle Faecher
    in Quarantaene, braucht die Kamera eine neue Region
    (`exhausted_by_quarantine`); die alte bleibt unberuehrt, bis der Knoten
    endet.
    """

    def __init__(self, slots: int) -> None:
        if slots < 1:
            raise ValueError("mindestens ein Fach")
        self._slots = slots
        self._free: List[int] = list(range(slots))
        self._quarantined: set = set()
        self._lock = threading.Lock()

    def acquire(self) -> Optional[int]:
        with self._lock:
            return self._free.pop(0) if self._free else None

    def release(self, slot: int) -> None:
        with self._lock:
            if slot not in self._free and slot not in self._quarantined:
                self._free.append(slot)

    def quarantine(self, slot: int) -> None:
        """Das Fach wird nie wieder vergeben: sein Leser ist nicht sicher fertig."""
        with self._lock:
            if slot in self._free:
                self._free.remove(slot)
            self._quarantined.add(slot)

    def settle(self, slot: int, outcome: "Outcome") -> bool:
        """Gibt das Fach frei, wenn der Ausgang sein Ende belegt.

        Gibt zurueck, ob es freigegeben wurde; sonst ist es in Quarantaene.
        """
        if outcome.ends_reading:
            self.release(slot)
            return True
        self.quarantine(slot)
        return False

    @property
    def free(self) -> int:
        with self._lock:
            return len(self._free)

    @property
    def quarantined(self) -> int:
        with self._lock:
            return len(self._quarantined)

    @property
    def exhausted_by_quarantine(self) -> bool:
        """Jedes Fach ist in Quarantaene; diese Region nimmt nichts mehr an."""
        with self._lock:
            return len(self._quarantined) == self._slots


def is_local_endpoint(endpoint: str) -> bool:
    """Liegt der Governor auf demselben Rechner?

    Nur dann teilen Bruecke und Governor ein `/dev/shm`. Eine Adresse, die
    nur zufaellig lokal aussieht (ein Portforward in einen anderen
    Container ohne `--ipc=host`), muss der Betreiber mit `transport: copy`
    ausschliessen.
    """
    host = endpoint.rsplit(":", 1)[0].strip("[]")
    return host in ("localhost", "127.0.0.1", "::1") or host.startswith("127.")


def parse_cameras(
    topics: Sequence[str],
    models: Sequence[str],
    groups: Sequence[str],
    keys: Iterable[int],
) -> List["Camera"]:
    """Liest die parallelen Parameterlisten zu Kamerabeschreibungen.

    ROS-2-Parameter kennen keine verschachtelten Listen; parallele Listen
    sind die uebliche Form. Ungleich lange Listen sind ein
    Konfigurationsfehler und werden abgelehnt, nicht aufgefuellt.
    """
    keys = list(keys)
    if not topics:
        raise ValueError("keine Kamera konfiguriert (topics ist leer)")
    if not (len(topics) == len(models) == len(groups) == len(keys)):
        raise ValueError(
            "topics, models, capture_groups und supersession_keys "
            "muessen gleich lang sein"
        )
    return [
        Camera(topic=t, model=m, group=g, supersession_key=k)
        for t, m, g, k in zip(topics, models, groups, keys)
    ]


@dataclasses.dataclass(frozen=True)
class Camera:
    """Eine Kamera: Topic, Modell, Aufnahmegruppe, Supersession-Schluessel."""

    topic: str
    model: str
    group: str
    supersession_key: int
