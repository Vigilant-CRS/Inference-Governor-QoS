"""Ein Stub-OIP-Server, der sich an den entscheidenden Stellen wie der
Governor verhaelt.

Er zeichnet jeden Request auf, liest Shared-Memory-Regionen tatsaechlich
aus, antwortet mit `vig_obsolete` oder lehnt mit `vig-reason` ab — nach
einer Regel, die der Test vorgibt. Kein Ersatz fuer den echten Governor,
sondern die Gegenstelle, gegen die sich die Bruecke deterministisch pruefen
laesst.
"""

from __future__ import annotations

import threading
from concurrent import futures
from multiprocessing import shared_memory
from typing import Callable, Dict, List, Optional

import grpc
import numpy as np
from tritonclient.grpc import service_pb2, service_pb2_grpc

INPUT_SHAPE = (1, 3, 8, 8)


class Recorded:
    """Was der Stub von einem Request gesehen hat."""

    def __init__(self, request, tensor: Optional[np.ndarray], via_shm: bool) -> None:
        self.id = request.id
        self.model = request.model_name
        self.params = {k: v.int64_param for k, v in request.parameters.items()}
        self.tensor = tensor
        self.via_shm = via_shm


class StubGovernor(service_pb2_grpc.GRPCInferenceServiceServicer):
    def __init__(self, decide: Optional[Callable[[Recorded], Optional[str]]] = None) -> None:
        self.requests: List[Recorded] = []
        self.regions: Dict[str, shared_memory.SharedMemory] = {}
        self.registered: Dict[str, int] = {}
        self._decide = decide or (lambda r: None)
        self._lock = threading.Lock()

    # --- Metadaten und Bereitschaft -------------------------------------

    def ServerReady(self, request, context):
        return service_pb2.ServerReadyResponse(ready=True)

    def ModelMetadata(self, request, context):
        response = service_pb2.ModelMetadataResponse(name=request.name)
        tensor = response.inputs.add()
        tensor.name, tensor.datatype = "images", "FP32"
        tensor.shape.extend(INPUT_SHAPE)
        out = response.outputs.add()
        out.name, out.datatype = "scores", "FP32"
        out.shape.extend([1, 4])
        return response

    # --- Shared Memory ---------------------------------------------------

    def SystemSharedMemoryRegister(self, request, context):
        with self._lock:
            self.regions[request.name] = shared_memory.SharedMemory(name=request.key.lstrip("/"))
            self.registered[request.name] = request.byte_size
        return service_pb2.SystemSharedMemoryRegisterResponse()

    def SystemSharedMemoryUnregister(self, request, context):
        with self._lock:
            region = self.regions.pop(request.name, None)
            self.registered.pop(request.name, None)
        if region is not None:
            region.close()
        return service_pb2.SystemSharedMemoryUnregisterResponse()

    # --- Inferenz -----------------------------------------------------------

    def ModelInfer(self, request, context):
        tensor_input = request.inputs[0]
        via_shm = "shared_memory_region" in tensor_input.parameters
        if via_shm:
            name = tensor_input.parameters["shared_memory_region"].string_param
            offset = tensor_input.parameters["shared_memory_offset"].int64_param
            size = tensor_input.parameters["shared_memory_byte_size"].int64_param
            with self._lock:
                region = self.regions[name]
            raw = bytes(region.buf[offset : offset + size])
        else:
            raw = request.raw_input_contents[0]
        tensor = np.frombuffer(raw, dtype=np.float32).reshape(tuple(tensor_input.shape))
        recorded = Recorded(request, tensor, via_shm)
        with self._lock:
            self.requests.append(recorded)

        verdict = self._decide(recorded)
        if verdict in ("superseded", "stale"):
            context.set_trailing_metadata((("vig-reason", verdict),))
            context.abort(grpc.StatusCode.ABORTED, verdict)
        if verdict == "infeasible":
            context.set_trailing_metadata((("vig-reason", verdict),))
            context.abort(grpc.StatusCode.RESOURCE_EXHAUSTED, verdict)

        response = service_pb2.ModelInferResponse(model_name=request.model_name, id=request.id)
        out = response.outputs.add()
        out.name, out.datatype = "scores", "FP32"
        out.shape.extend([1, 4])
        response.raw_output_contents.append(np.array([[float(tensor.mean()), 1, 2, 3]], dtype=np.float32).tobytes())
        if verdict == "obsolete":
            response.parameters["vig_obsolete"].bool_param = True
        return response

    def close(self) -> None:
        for region in self.regions.values():
            region.close()


def start(decide=None):
    """Startet den Stub auf einem freien Port. Gibt (Server, Stub, Endpunkt)."""
    stub = StubGovernor(decide)
    server = grpc.server(futures.ThreadPoolExecutor(max_workers=8))
    service_pb2_grpc.add_GRPCInferenceServiceServicer_to_server(stub, server)
    port = server.add_insecure_port("127.0.0.1:0")
    server.start()
    return server, stub, f"127.0.0.1:{port}"
