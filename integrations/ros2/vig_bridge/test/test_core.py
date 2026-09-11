"""Die Logik der Bruecke, ohne ROS und ohne gRPC."""

import threading

import pytest

from vig_bridge.core import (
    CaptureIds,
    FreshnessParams,
    Outcome,
    RequestIds,
    SlotRing,
    TensorSpec,
    age_of,
    classify_error,
    image_to_tensor,
    is_local_endpoint,
    parse_cameras,
)

MS = 1_000_000


# --- Alter ------------------------------------------------------------------


def test_age_is_now_minus_stamp_in_microseconds():
    age = age_of(stamp_ns=1_000 * MS, now_ns=1_012 * MS)
    assert age.micros == 12_000
    assert not age.clock_skew


def test_a_stamp_slightly_in_the_future_is_clamped_not_reported():
    # 2 ms Versatz ist normal zwischen zwei Rechnern mit chrony.
    age = age_of(stamp_ns=1_002 * MS, now_ns=1_000 * MS, max_clock_skew_us=10_000)
    assert age.micros == 0
    assert not age.clock_skew


def test_a_stamp_far_in_the_future_is_clamped_and_reported():
    # Ein negatives Alter still zu verrechnen hiesse, einen frischeren Frame
    # zu versprechen, als es ihn gibt.
    age = age_of(stamp_ns=1_500 * MS, now_ns=1_000 * MS, max_clock_skew_us=10_000)
    assert age.micros == 0
    assert age.clock_skew


# --- Kennungen ----------------------------------------------------------------


def test_request_ids_are_numeric_unique_and_carry_the_process():
    ids = RequestIds(process_tag=7)
    first, second = ids.next(), ids.next()
    assert first != second
    assert first >> 32 == 7 and second >> 32 == 7
    assert 0 < first < 2**63


def test_request_ids_of_two_bridges_do_not_collide():
    a, b = RequestIds(process_tag=1), RequestIds(process_tag=2)
    assert {a.next() for _ in range(100)}.isdisjoint({b.next() for _ in range(100)})


def test_request_ids_are_unique_across_threads():
    ids = RequestIds(process_tag=3)
    seen = []

    def take():
        seen.extend(ids.next() for _ in range(500))

    threads = [threading.Thread(target=take) for _ in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert len(set(seen)) == 2_000


def test_two_cameras_triggered_together_share_one_capture():
    captures = CaptureIds(tolerance_ns=1 * MS)
    left = captures.assign("stereo", 100 * MS)
    right = captures.assign("stereo", 100 * MS + 300_000)  # 0,3 ms spaeter
    assert left == right


def test_the_next_trigger_is_a_new_capture():
    captures = CaptureIds(tolerance_ns=1 * MS)
    first = captures.assign("stereo", 100 * MS)
    second = captures.assign("stereo", 133 * MS)
    assert first != second


def test_different_groups_never_share_a_capture():
    captures = CaptureIds(tolerance_ns=1 * MS)
    assert captures.assign("front", 100 * MS) != captures.assign("rear", 100 * MS)


def test_capture_memory_is_bounded():
    captures = CaptureIds(tolerance_ns=1 * MS, memory=4)
    first = captures.assign("g", 0)
    for i in range(1, 10):
        captures.assign("g", i * 100 * MS)
    # Der erste Stempel ist aus dem Gedaechtnis gefallen: dieselbe Zeit ergibt
    # jetzt eine neue Aufnahme statt einer erfundenen Zuordnung.
    assert captures.assign("g", 0) != first


# --- Parameter ----------------------------------------------------------------


def test_only_set_parameters_are_sent():
    params = FreshnessParams(age_us=12_000, capture_id=41, supersession_key=3).as_dict()
    assert params == {"vig_age_us": 12_000, "vig_capture_id": 41, "vig_supersession_key": 3}


def test_without_an_age_the_governor_counts_from_arrival():
    assert "vig_age_us" not in FreshnessParams(age_us=None, capture_id=1).as_dict()


def test_values_outside_int64_are_refused():
    with pytest.raises(ValueError):
        FreshnessParams(age_us=-1).as_dict()
    with pytest.raises(ValueError):
        FreshnessParams(age_us=2**63).as_dict()


# --- Ablehnungen --------------------------------------------------------------


@pytest.mark.parametrize(
    "code, reason, expected",
    [
        ("ABORTED", "superseded", Outcome.SUPERSEDED),
        ("ABORTED", "stale", Outcome.STALE),
        ("RESOURCE_EXHAUSTED", "infeasible", Outcome.INFEASIBLE),
        ("RESOURCE_EXHAUSTED", None, Outcome.CAPACITY),
        ("FAILED_PRECONDITION", None, Outcome.FUSION_REFUSED),
        ("INVALID_ARGUMENT", None, Outcome.INVALID),
        ("NOT_FOUND", None, Outcome.NOT_FOUND),
        ("UNAVAILABLE", "backend_failed", Outcome.BACKEND_FAILED),
        ("UNAVAILABLE", "execution_unknown", Outcome.EXECUTION_UNKNOWN),
        ("DEADLINE_EXCEEDED", "backend_timeout", Outcome.BACKEND_TIMEOUT),
        ("UNAVAILABLE", None, Outcome.TRANSPORT),
        ("StatusCode.ABORTED", "stale", Outcome.STALE),
        ("FAILED_PRECONDITION", "capture_mismatch", Outcome.FUSION_REFUSED),
        ("FAILED_PRECONDITION", "graph_full", Outcome.CAPACITY),
    ],
)
def test_refusals_are_classified_by_reason_before_code(code, reason, expected):
    assert classify_error(code, reason) is expected


def test_a_refusal_is_information_not_an_error():
    assert Outcome.STALE.is_refusal and Outcome.SUPERSEDED.is_refusal
    assert not Outcome.BACKEND_FAILED.is_refusal
    assert not Outcome.DELIVERED.is_refusal


def test_only_a_transport_error_warrants_resending_the_same_frame():
    assert Outcome.TRANSPORT.retry_same_frame
    assert not Outcome.STALE.retry_same_frame
    assert not Outcome.CAPACITY.retry_same_frame


# --- Bild -> Tensor -----------------------------------------------------------


def _rgb(width, height, value=(10, 20, 30)):
    return bytes(value) * (width * height)


def test_rgb8_becomes_nchw_float_in_zero_to_one():
    spec = TensorSpec("images", "FP32", (1, 3, 4, 4))
    tensor = image_to_tensor(_rgb(8, 8), 8, 8, "rgb8", 24, spec)
    assert tensor.shape == (1, 3, 4, 4)
    assert tensor.dtype.name == "float32"
    assert tensor[0, 0, 0, 0] == pytest.approx(10 / 255)
    assert tensor[0, 2, 0, 0] == pytest.approx(30 / 255)


def test_bgr8_is_reordered_to_rgb():
    spec = TensorSpec("images", "FP32", (1, 3, 2, 2))
    tensor = image_to_tensor(_rgb(2, 2, (30, 20, 10)), 2, 2, "bgr8", 6, spec)
    assert tensor[0, 0, 0, 0] == pytest.approx(10 / 255)


def test_row_padding_in_step_is_ignored():
    spec = TensorSpec("images", "UINT8", (1, 2, 2, 3), layout="NHWC")
    row = bytes([1, 2, 3, 4, 5, 6]) + b"\xff\xff"  # zwei Pixel, zwei Fuellbytes
    tensor = image_to_tensor(row * 2, 2, 2, "rgb8", 8, spec)
    assert tensor.shape == (1, 2, 2, 3)
    assert tensor.max() == 6


def test_a_fixed_batch_is_refused_unless_filling_is_asked_for():
    spec = TensorSpec("data", "FP32", (4, 3, 2, 2))
    with pytest.raises(ValueError):
        image_to_tensor(_rgb(2, 2), 2, 2, "rgb8", 6, spec)
    tensor = image_to_tensor(_rgb(2, 2), 2, 2, "rgb8", 6, spec, fill_batch=True)
    assert tensor.shape == (4, 3, 2, 2)


def test_an_unknown_encoding_is_refused():
    with pytest.raises(ValueError):
        image_to_tensor(b"\x00" * 16, 2, 2, "yuv422", 4, TensorSpec("x", "FP32", (1, 3, 2, 2)))


def test_the_tensor_size_matches_the_spec_byte_size():
    spec = TensorSpec("images", "FP32", (1, 3, 16, 16))
    assert image_to_tensor(_rgb(32, 24), 32, 24, "rgb8", 96, spec).nbytes == spec.byte_size


# --- Shared-Memory-Faecher ----------------------------------------------------


def test_a_slot_is_reused_only_after_its_request_is_answered():
    ring = SlotRing(2)
    a, b = ring.acquire(), ring.acquire()
    assert {a, b} == {0, 1}
    assert ring.acquire() is None, "alle Faecher belegt ist Backpressure, kein Ueberschreiben"
    ring.release(a)
    assert ring.acquire() == a


def test_releasing_twice_does_not_create_a_phantom_slot():
    ring = SlotRing(1)
    slot = ring.acquire()
    ring.release(slot)
    ring.release(slot)
    assert ring.free == 1


@pytest.mark.parametrize(
    "endpoint, local",
    [("127.0.0.1:9001", True), ("localhost:9001", True), ("[::1]:9001", True), ("10.0.0.5:9001", False), ("governor:9001", False)],
)
def test_only_loopback_counts_as_the_same_host(endpoint, local):
    assert is_local_endpoint(endpoint) is local


# --- Konfiguration ------------------------------------------------------------


def test_cameras_come_from_parallel_lists():
    cameras = parse_cameras(["/front/image", "/rear/image"], ["det", "det"], ["a", "b"], [1, 2])
    assert [c.supersession_key for c in cameras] == [1, 2]


def test_lists_of_different_length_are_refused_not_padded():
    with pytest.raises(ValueError):
        parse_cameras(["/front/image", "/rear/image"], ["det"], ["a", "b"], [1, 2])
    with pytest.raises(ValueError):
        parse_cameras([], [], [], [])
