"""Der Transport zum Governor: Open Inference Protocol ueber gRPC.

Bewusst ueber die generierten Stubs aus `tritonclient` und nicht ueber
dessen `InferenceServerClient`: der Governor nennt den Grund einer
Ablehnung im Trailing-Metadatum `vig-reason`, und die Ausnahme des
komfortablen Clients reicht Trailing-Metadaten nicht durch. Ohne sie waere
„ueberholt" von „zu alt" nur ueber den Fehlertext zu unterscheiden.

Kein natives Stueck Code: gRPC und Protobuf sind Python-Pakete, Shared
Memory kommt aus `multiprocessing.shared_memory` (ADR-0033).
"""

from __future__ import annotations

import dataclasses
import time
from multiprocessing import shared_memory
from typing import Callable, Dict, Optional

from .core import Outcome, TensorSpec, classify_error

MAX_MESSAGE_BYTES = 64 * 1024 * 1024

_NUMPY_TYPES = {
    "FP32": "float32",
    "FP16": "float16",
    "FP64": "float64",
    "INT64": "int64",
    "INT32": "int32",
    "INT8": "int8",
    "UINT8": "uint8",
    "BOOL": "bool",
}


@dataclasses.dataclass
class InferResult:
    """Wie ein Request endete, mit den Ausgaben, falls es welche gab."""

    outcome: Outcome
    outputs: Dict[str, object] = dataclasses.field(default_factory=dict)
    detail: str = ""
    latency_us: int = 0


class ShmRegion:
    """Eine System-Shared-Memory-Region mit mehreren Faechern.

    Eine Region je Kamera, `slots` Faecher darin. Der Governor und das
    Backend lesen ueber `shared_memory_offset` das Fach, das der jeweilige
    Request nennt; die Bruecke vergibt ein Fach erst wieder, wenn sein
    Request beantwortet ist (`core.SlotRing`).
    """

    def __init__(self, name: str, slot_bytes: int, slots: int) -> None:
        self.name = name
        self.slot_bytes = slot_bytes
        self.slots = slots
        self.byte_size = slot_bytes * slots
        try:
            stale = shared_memory.SharedMemory(name=name)
            stale.close()
            stale.unlink()
        except FileNotFoundError:
            pass
        self._shm = shared_memory.SharedMemory(name=name, create=True, size=self.byte_size)

    @property
    def key(self) -> str:
        """Der `shm_open`-Schluessel, den Governor und Backend oeffnen."""
        return "/" + self.name

    def write(self, slot: int, data: bytes) -> int:
        """Schreibt einen Tensor in ein Fach und gibt den Offset zurueck."""
        if len(data) > self.slot_bytes:
            raise ValueError("Tensor groesser als ein Fach")
        offset = slot * self.slot_bytes
        self._shm.buf[offset : offset + len(data)] = data
        return offset

    def close(self) -> None:
        self._shm.close()
        try:
            self._shm.unlink()
        except FileNotFoundError:
            pass


class GovernorClient:
    """Ein OIP-Client, der die Antworten des Governors vollstaendig liest."""

    def __init__(self, endpoint: str, timeout_s: float = 5.0, token: Optional[str] = None) -> None:
        import grpc
        from tritonclient.grpc import service_pb2, service_pb2_grpc

        self._grpc = grpc
        self._pb = service_pb2
        self._timeout_s = timeout_s
        self._metadata = (("authorization", f"Bearer {token}"),) if token else None
        self._channel = grpc.insecure_channel(
            endpoint,
            options=[
                ("grpc.max_send_message_length", MAX_MESSAGE_BYTES),
                ("grpc.max_receive_message_length", MAX_MESSAGE_BYTES),
            ],
        )
        self._stub = service_pb2_grpc.GRPCInferenceServiceStub(self._channel)

    def close(self) -> None:
        self._channel.close()

    def wait_ready(self, timeout_s: float = 10.0) -> None:
        """Wartet, bis der Governor antwortet. Wirft, wenn er es nicht tut."""
        self._grpc.channel_ready_future(self._channel).result(timeout=timeout_s)

    def input_spec(self, model: str, layout: str = "NCHW") -> TensorSpec:
        """Die erste Eingabe eines Modells, wie der Governor sie meldet.

        Eine dynamische Batchdimension (`-1`) wird als eins gelesen: die
        Bruecke schickt je Frame genau ein Bild.
        """
        response = self._stub.ModelMetadata(
            self._pb.ModelMetadataRequest(name=model),
            timeout=self._timeout_s,
            metadata=self._metadata,
        )
        first = response.inputs[0]
        shape = tuple(1 if (d < 0 and i == 0) else int(d) for i, d in enumerate(first.shape))
        if len(shape) == 3:
            shape = (1,) + shape
        return TensorSpec(name=first.name, datatype=first.datatype, shape=shape, layout=layout)

    def register_region(self, region: ShmRegion) -> None:
        # Eine Region gleichen Namens aus einem abgebrochenen Lauf zuerst
        # abmelden; sonst lehnt das Backend die Registrierung ab.
        try:
            self._stub.SystemSharedMemoryUnregister(
                self._pb.SystemSharedMemoryUnregisterRequest(name=region.name),
                timeout=self._timeout_s,
                metadata=self._metadata,
            )
        except self._grpc.RpcError:
            pass
        self._stub.SystemSharedMemoryRegister(
            self._pb.SystemSharedMemoryRegisterRequest(
                name=region.name, key=region.key, offset=0, byte_size=region.byte_size
            ),
            timeout=self._timeout_s,
            metadata=self._metadata,
        )

    def unregister_region(self, region: ShmRegion) -> None:
        try:
            self._stub.SystemSharedMemoryUnregister(
                self._pb.SystemSharedMemoryUnregisterRequest(name=region.name),
                timeout=self._timeout_s,
                metadata=self._metadata,
            )
        except self._grpc.RpcError:
            pass

    def build_request(
        self,
        model: str,
        spec: TensorSpec,
        request_id: int,
        parameters: Dict[str, int],
        tensor=None,
        region: Optional[ShmRegion] = None,
        offset: int = 0,
    ):
        """Baut den `ModelInferRequest`: Kopie oder Shared-Memory-Referenz."""
        pb = self._pb
        request = pb.ModelInferRequest(model_name=model, id=str(request_id))
        for name, value in parameters.items():
            request.parameters[name].int64_param = value
        tensor_input = request.inputs.add()
        tensor_input.name = spec.name
        tensor_input.datatype = spec.datatype
        tensor_input.shape.extend(spec.shape)
        if region is not None:
            tensor_input.parameters["shared_memory_region"].string_param = region.name
            tensor_input.parameters["shared_memory_byte_size"].int64_param = spec.byte_size
            tensor_input.parameters["shared_memory_offset"].int64_param = offset
        else:
            if tensor is None:
                raise ValueError("ohne Shared Memory braucht der Request einen Tensor")
            request.raw_input_contents.append(tensor.tobytes())
        return request

    def infer_async(self, request, callback: Callable[[InferResult], None]) -> None:
        """Schickt den Request und ruft `callback` mit dem Ergebnis auf.

        Der Callback laeuft in einem gRPC-Thread.
        """
        sent = time.monotonic_ns()
        future = self._stub.ModelInfer.future(
            request, timeout=self._timeout_s, metadata=self._metadata
        )

        def done(f) -> None:
            result = self.read_result(f)
            result.latency_us = (time.monotonic_ns() - sent) // 1_000
            callback(result)

        future.add_done_callback(done)

    def read_result(self, future) -> InferResult:
        """Liest eine beendete gRPC-Antwort: Ergebnis oder Ablehnung."""
        error = future.exception()
        if error is not None:
            code = error.code().name if hasattr(error, "code") else "UNKNOWN"
            reason = None
            trailing = error.trailing_metadata() if hasattr(error, "trailing_metadata") else None
            for key, value in trailing or ():
                if key == "vig-reason":
                    reason = value
            details = error.details() if hasattr(error, "details") else str(error)
            return InferResult(outcome=classify_error(code, reason, details), detail=details or "")
        response = future.result()
        return InferResult(outcome=_delivered(response), outputs=decode_outputs(response))


def _delivered(response) -> Outcome:
    """Geliefert — und ob das Ergebnis bei Fertigstellung schon veraltet war."""
    obsolete = response.parameters.get("vig_obsolete")
    if obsolete is not None and obsolete.bool_param:
        return Outcome.DELIVERED_OBSOLETE
    return Outcome.DELIVERED


def decode_outputs(response) -> Dict[str, object]:
    """Die Ausgabetensoren als numpy-Arrays."""
    import numpy as np

    outputs: Dict[str, object] = {}
    for index, output in enumerate(response.outputs):
        dtype = _NUMPY_TYPES.get(output.datatype)
        if dtype is None or index >= len(response.raw_output_contents):
            continue
        array = np.frombuffer(response.raw_output_contents[index], dtype=dtype)
        outputs[output.name] = array.reshape(tuple(output.shape))
    return outputs
