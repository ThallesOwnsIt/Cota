"""Formatos canônicos compartilhados com o Rust (`cota-core`).

- `reading_bytes`: Borsh de `Reading`, a folha da árvore de cada lote.
- `merkle_root`: RFC 9162 com SHA-256, folha H(0x00‖d) e nó H(0x01‖l‖r).
"""

from __future__ import annotations

import hashlib
import struct

SOURCES = {"inmet": 0, "open_meteo_glofas": 1, "ana": 2, "cemaden": 3, "public_record": 4}
VARIABLES = {"precipitation": 0, "river_stage": 1, "river_discharge": 2}


def _borsh_str(s: str) -> bytes:
    b = s.encode()
    return struct.pack("<I", len(b)) + b


def reading_bytes(r: dict) -> bytes:
    """Mesmos bytes de `Reading::canonical_bytes` (ordem dos campos da struct)."""
    out = _borsh_str(r["station_id"])
    out += struct.pack("<B", VARIABLES[r["variable"]])
    out += struct.pack("<q", r["ts_utc"])
    out += struct.pack("<B", SOURCES[r["source"]])
    out += struct.pack("<q", r["value_milli"])
    out += b"\x00" if r.get("datum_id") is None else b"\x01" + _borsh_str(r["datum_id"])
    out += bytes.fromhex(r["raw_payload_sha256"])
    return out


def hash_leaf(data: bytes) -> bytes:
    return hashlib.sha256(b"\x00" + data).digest()


def hash_node(left: bytes, right: bytes) -> bytes:
    return hashlib.sha256(b"\x01" + left + right).digest()


def _root(leaves: list[bytes]) -> bytes:
    n = len(leaves)
    if n == 0:
        return hashlib.sha256(b"").digest()
    if n == 1:
        return leaves[0]
    k = 1 << ((n - 1).bit_length() - 1)  # maior potência de 2 < n
    return hash_node(_root(leaves[:k]), _root(leaves[k:]))


def merkle_root(items: list[bytes]) -> bytes:
    return _root([hash_leaf(d) for d in items])


def feature_leaf(name: str, value_milli: int | None) -> bytes:
    """Folha de uma feature comprometida: `nome=valor_milli`, ou `nome=null` se faltante."""
    return f"{name}={'null' if value_milli is None else value_milli}".encode()
