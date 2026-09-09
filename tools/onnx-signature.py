#!/usr/bin/env python3
"""Liest die I/O-Signatur eines ONNX-Modells — ohne ONNX-Abhaengigkeit.

Wozu
----

Der Governor waehlt die Modellvariante je Request und sagt es dem Client
nicht. Diese Freiheit setzt voraus, dass alle Varianten dieselbe
Schnittstelle bedienen. Gleiche Namen, Typen und Formen sind dafuer die
**notwendige** Bedingung; ob zwei Ausgaben auch dasselbe *bedeuten*, kann
kein Werkzeug pruefen — das sagt die `io_signature` in der Konfiguration.

Dieses Skript beantwortet die pruefbare Haelfte, bevor irgendetwas laeuft:

    tools/onnx-signature.py modell_a.onnx modell_b.onnx

Es meldet die Signaturen und, bei mehreren Dateien, ob sie uebereinstimmen.

Warum ein eigener Parser
------------------------

`onnx` und `onnxruntime` sind schwere Abhaengigkeiten fuer eine Frage, die
sich aus zwei Protobuf-Feldern beantworten laesst. Der Leser hier steigt
gezielt in `ModelProto.graph.input` und `.output` ab und ueberspringt alles
andere ueber seine Laengenangabe — die Gewichte werden nie gelesen. Ein
133-MB-Modell kostet damit Millisekunden statt eines Imports.
"""

from __future__ import annotations

import sys
from pathlib import Path

# TensorProto.DataType -> Bezeichner des Open Inference Protocol. Triton und
# der Governor sprechen OIP; die ONNX-eigenen Namen waeren hier eine zweite
# Schreibweise fuer dieselbe Sache.
OIP_TYPE = {
    1: "FP32", 2: "UINT8", 3: "INT8", 4: "UINT16", 5: "INT16",
    6: "INT32", 7: "INT64", 8: "BYTES", 9: "BOOL", 10: "FP16",
    11: "FP64", 12: "UINT32", 13: "UINT64", 16: "BF16",
}


def read_varint(buf: memoryview, pos: int) -> tuple[int, int]:
    """Liest ein Varint und gibt (Wert, neue Position) zurueck."""
    result = 0
    shift = 0
    while True:
        byte = buf[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, pos
        shift += 7
        if shift > 70:
            raise ValueError("Varint ohne Ende")


def fields(buf: memoryview, start: int, end: int):
    """Iteriert ueber (Feldnummer, Wiretyp, Nutzdaten) einer Nachricht.

    Ueberspringt jedes Feld ueber seine Laengenangabe. Genau das macht den
    Leser billig: die Gewichte eines Modells werden nie angefasst.
    """
    pos = start
    while pos < end:
        tag, pos = read_varint(buf, pos)
        number, wire = tag >> 3, tag & 7
        if wire == 0:
            value, pos = read_varint(buf, pos)
            yield number, wire, value
        elif wire == 1:
            yield number, wire, buf[pos:pos + 8]
            pos += 8
        elif wire == 2:
            length, pos = read_varint(buf, pos)
            yield number, wire, (pos, pos + length)
            pos += length
        elif wire == 5:
            yield number, wire, buf[pos:pos + 4]
            pos += 4
        else:
            raise ValueError(f"unbekannter Wiretyp {wire}")


def parse_value_info(buf: memoryview, start: int, end: int) -> tuple[str, str, list]:
    """ValueInfoProto -> (Name, Datentyp, Dimensionen)."""
    name = ""
    datatype = "?"
    dims: list = []
    for number, wire, payload in fields(buf, start, end):
        if number == 1 and wire == 2:            # name
            a, b = payload
            name = bytes(buf[a:b]).decode("utf-8", "replace")
        elif number == 2 and wire == 2:          # type: TypeProto
            a, b = payload
            for n2, w2, p2 in fields(buf, a, b):
                if n2 == 1 and w2 == 2:          # tensor_type
                    c, d = p2
                    for n3, w3, p3 in fields(buf, c, d):
                        if n3 == 1 and w3 == 0:  # elem_type
                            datatype = OIP_TYPE.get(p3, f"TYPE_{p3}")
                        elif n3 == 2 and w3 == 2:  # shape
                            e, f = p3
                            dims = parse_shape(buf, e, f)
    return name, datatype, dims


def parse_shape(buf: memoryview, start: int, end: int) -> list:
    """TensorShapeProto -> Liste aus Zahlen und benannten Achsen."""
    out: list = []
    for number, wire, payload in fields(buf, start, end):
        if number != 1 or wire != 2:             # dim
            continue
        a, b = payload
        entry: object = -1
        for n2, w2, p2 in fields(buf, a, b):
            if n2 == 1 and w2 == 0:              # dim_value
                entry = p2
            elif n2 == 2 and w2 == 2:            # dim_param
                c, d = p2
                entry = bytes(buf[c:d]).decode("utf-8", "replace")
        out.append(entry)
    return out


def initializer_name(buf: memoryview, start: int, end: int) -> str:
    """Der Name eines TensorProto — ohne seine Nutzdaten anzufassen."""
    for number, wire, payload in fields(buf, start, end):
        if number == 8 and wire == 2:            # TensorProto.name
            a, b = payload
            return bytes(buf[a:b]).decode("utf-8", "replace")
    return ""


def signature(path: Path) -> tuple[list, list]:
    """Die Ein- und Ausgaben eines ONNX-Modells, sortiert.

    Gewichte werden nicht mitgezaehlt, auch wenn das Modell sie als
    `graph.input` fuehrt: aeltere Exporte tun das, und ONNX erlaubt es. Wer
    sie mitzaehlt, meldet zwischen zwei ResNets hunderte Unterschiede, die
    keiner ist — Triton filtert sie ebenfalls heraus, und verglichen werden
    muss, was der Server meldet.
    """
    buf = memoryview(path.read_bytes())
    inputs: list = []
    outputs: list = []
    weights: set[str] = set()

    for number, wire, payload in fields(buf, 0, len(buf)):
        if number != 7 or wire != 2:             # ModelProto.graph
            continue
        a, b = payload
        for n2, w2, p2 in fields(buf, a, b):
            if w2 != 2:
                continue
            c, d = p2
            if n2 == 5:                          # graph.initializer
                weights.add(initializer_name(buf, c, d))
            elif n2 in (11, 12):                 # graph.input / graph.output
                entry = parse_value_info(buf, c, d)
                (inputs if n2 == 11 else outputs).append(entry)

    inputs = [e for e in inputs if e[0] not in weights]
    return sorted(inputs), sorted(outputs)


def render(entry: tuple[str, str, list]) -> str:
    """Dieselbe Normalform, die der Governor vergleicht."""
    name, datatype, dims = entry
    shown = ",".join("?" if isinstance(d, str) or d < 0 else str(d) for d in dims)
    return f"{name}:{datatype}[{shown}]"


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(__doc__)
        return 2

    paths = [Path(a) for a in argv[1:]]
    # Modellrepositorien legen jede Variante als `<name>/1/model.onnx` ab.
    # Nur den Dateinamen zu zeigen waere dort fuer jede Zeile derselbe.
    ambiguous = len({p.name for p in paths}) < len(paths)
    label = (
        (lambda p: "/".join(p.parts[-3:]) if len(p.parts) >= 3 else str(p))
        if ambiguous
        else (lambda p: p.name)
    )

    # Eine Liste und kein Dictionary: zweimal derselbe Pfad ist ein
    # gueltiger Aufruf, und er soll "gleich" melden statt den Vergleich
    # stillschweigend zu ueberspringen.
    signatures: list = []
    for path in paths:
        try:
            inputs, outputs = signature(path)
        except (OSError, ValueError) as error:
            print(f"{label(path)}: nicht lesbar ({error})")
            return 1
        normalised = ([render(e) for e in inputs], [render(e) for e in outputs])
        signatures.append((label(path), normalised))
        print(f"\n{label(path)}")
        for entry in inputs:
            print(f"  in   {render(entry)}")
        for entry in outputs:
            print(f"  out  {render(entry)}")

    if len(signatures) < 2:
        return 0

    # Der eigentliche Zweck: sind diese Varianten austauschbar?
    reference_label, reference = signatures[0]
    divergent = [name for name, sig in signatures[1:] if sig != reference]

    print()
    if not divergent:
        print("Alle Varianten haben dieselbe I/O-Signatur.")
        print("Notwendige Bedingung erfuellt — ob sie dasselbe *bedeuten*,")
        print("sagt keine Metadatenpruefung. Das gehoert in die io_signature.")
        return 0

    print(f"Abweichende Signatur gegenueber {reference_label}:")
    for name in divergent:
        print(f"  {name}")
    print()
    print("Der Governor schaltet die automatische Variantenwahl fuer ein")
    print("solches Modell ab. Wer sie will, braucht Adapter oder Varianten,")
    print("die dieselbe Schnittstelle bedienen.")
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
