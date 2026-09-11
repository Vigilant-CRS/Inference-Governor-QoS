"""Der Transport gegen einen Stub-Governor: Kopie, Shared Memory, Ablehnung."""

import threading
import uuid

import numpy as np
import pytest

pytest.importorskip("tritonclient.grpc")

from vig_bridge.core import FreshnessParams, Outcome  # noqa: E402
from vig_bridge.oip import GovernorClient, ShmRegion  # noqa: E402

from stub_governor import INPUT_SHAPE, start  # noqa: E402


@pytest.fixture
def governor():
    verdicts = {}
    server, stub, endpoint = start(lambda r: verdicts.get(r.id))
    client = GovernorClient(endpoint, timeout_s=5.0)
    client.wait_ready()
    yield client, stub, verdicts
    client.close()
    server.stop(0)
    stub.close()


def _infer(client, request):
    done = threading.Event()
    box = {}

    def callback(result):
        box["result"] = result
        done.set()

    client.infer_async(request, callback)
    assert done.wait(5.0), "keine Antwort vom Stub"
    return box["result"]


def _tensor(value=0.5):
    return np.full(INPUT_SHAPE, value, dtype=np.float32)


def test_the_input_spec_comes_from_the_governor(governor):
    client, _, _ = governor
    spec = client.input_spec("det")
    assert spec.name == "images" and spec.shape == INPUT_SHAPE and spec.datatype == "FP32"


def test_the_copy_path_carries_the_tensor_and_the_freshness_parameters(governor):
    client, stub, _ = governor
    spec = client.input_spec("det")
    params = FreshnessParams(age_us=12_000, capture_id=41, supersession_key=3).as_dict()
    result = _infer(client, client.build_request("det", spec, 4242, params, tensor=_tensor(0.25)))

    assert result.outcome is Outcome.DELIVERED
    assert result.outputs["scores"][0, 0] == pytest.approx(0.25)
    seen = stub.requests[-1]
    assert seen.id == "4242", "numerische id, sonst kann niemand diesen Request als Elternteil nennen"
    assert seen.params == {"vig_age_us": 12_000, "vig_capture_id": 41, "vig_supersession_key": 3}
    assert not seen.via_shm


def test_the_shared_memory_path_carries_the_data_and_no_payload(governor):
    client, stub, _ = governor
    spec = client.input_spec("det")
    region = ShmRegion(f"vig_bridge_test_{uuid.uuid4().hex[:8]}", spec.byte_size, 2)
    try:
        client.register_region(region)
        assert stub.registered[region.name] == region.byte_size
        offset = region.write(1, _tensor(0.75).tobytes())
        request = client.build_request("det", spec, 7, {}, region=region, offset=offset)
        assert not request.raw_input_contents, "auf dem Shared-Memory-Pfad reist keine Nutzlast"

        result = _infer(client, request)
        assert result.outcome is Outcome.DELIVERED
        assert stub.requests[-1].via_shm
        assert stub.requests[-1].tensor.mean() == pytest.approx(0.75), "das zweite Fach, nicht das erste"
        client.unregister_region(region)
        assert region.name not in stub.registered
    finally:
        region.close()


@pytest.mark.parametrize(
    "verdict, outcome",
    [("stale", Outcome.STALE), ("superseded", Outcome.SUPERSEDED), ("infeasible", Outcome.INFEASIBLE)],
)
def test_a_refusal_arrives_with_its_reason(governor, verdict, outcome):
    client, _, verdicts = governor
    verdicts["9"] = verdict
    spec = client.input_spec("det")
    result = _infer(client, client.build_request("det", spec, 9, {}, tensor=_tensor()))
    assert result.outcome is outcome
    assert result.outcome.is_refusal
    assert not result.outputs


def test_an_obsolete_result_is_delivered_and_marked(governor):
    client, _, verdicts = governor
    verdicts["11"] = "obsolete"
    spec = client.input_spec("det")
    result = _infer(client, client.build_request("det", spec, 11, {}, tensor=_tensor()))
    assert result.outcome is Outcome.DELIVERED_OBSOLETE
    assert "scores" in result.outputs, "veraltet heisst geliefert und markiert, nicht verschwiegen"


def test_an_unreachable_governor_is_a_transport_error_not_a_refusal():
    client = GovernorClient("127.0.0.1:1", timeout_s=0.5)
    try:
        from vig_bridge.core import TensorSpec

        spec = TensorSpec("images", "FP32", INPUT_SHAPE)
        result = _infer(client, client.build_request("det", spec, 1, {}, tensor=_tensor()))
        assert result.outcome is Outcome.TRANSPORT
        assert result.outcome.retry_same_frame
    finally:
        client.close()
