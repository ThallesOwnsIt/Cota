#!/usr/bin/env python3
"""Gera o mapa da bacia do Guaíba usado no painel (dashboard/basin-map.json).

    python3 scripts/build_map.py <pasta-com-os-dados-brutos>

Roda uma vez; o resultado é versionado. Precisa de `pyshp` e `shapely`. A pasta deve ter:
- HydroBASINS nível 7, América do Sul: basins/hybas_sa_lev07_v1c.shp
  https://data.hydrosheds.org/file/HydroBASINS/standard/hybas_sa_lev07_v1c.zip
- HydroRIVERS, América do Sul: rivers/HydroRIVERS_v10_sa_shp/HydroRIVERS_v10_sa.shp
  https://data.hydrosheds.org/file/HydroRIVERS/HydroRIVERS_v10_sa_shp.zip
- Natural Earth 1:10m (domínio público): ne/ne_10m_lakes.shp, ne/ne_10m_coastline.shp,
  ne/ne_10m_admin_1_states_provinces_lakes.shp

HydroSHEDS: Lehner & Grill (2013), www.hydrosheds.org. Uso livre com atribuição.

A bacia é tudo o que drena para a régua do Cais Mauá: o conjunto Jacuí + Taquari, o conjunto
Caí + Sinos e o Gravataí (82.591 km², contra 82.400 km² da área de drenagem que a ANA informa
para a estação 87450004). As coordenadas saem já projetadas (equirretangular com o cosseno da
latitude central), em unidades de SVG; o painel usa a mesma projeção para pôr as estações.
"""

from __future__ import annotations

import json
import math
import sys
from pathlib import Path

import shapefile
from shapely.geometry import MultiLineString, Polygon, box, shape
from shapely.ops import linemerge, unary_union

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "dashboard/basin-map.json"

# Bacias de saída no HydroBASINS nível 7; a bacia é a união delas com tudo o que drena para elas.
OUTLETS = {6070015560: "jacui", 6070015530: "cai-sinos", 6070015510: "gravatai"}
# Recorte do mapa (lon/lat): a bacia inteira, mais o Guaíba e o começo da Lagoa dos Patos.
FRAME = (-54.75, -31.0, -49.75, -27.95)
WIDTH = 1000
# Rios com pelo menos esta área a montante entram no mapa (km²); a espessura cresce com ela.
MIN_UPLAND = 1500
# Rótulos dos rios (lon, lat), postos à mão perto do curso principal.
LABELS = [
    ("Jacuí", -53.05, -29.93),
    ("Taquari", -51.83, -29.12),
    ("Antas", -50.95, -28.83),
    ("Caí", -51.38, -29.47),
    ("Sinos", -50.75, -29.66),
    ("Gravataí", -50.82, -29.99),
    ("Guaíba", -51.33, -30.22),
    ("Lagoa dos Patos", -51.02, -30.86),
]


def read(path: Path, bbox=None):
    r = shapefile.Reader(str(path))
    names = [f[0] for f in r.fields[1:]]
    for sr in r.iterShapeRecords():
        if bbox is not None:
            b = sr.shape.bbox
            if b[2] < bbox[0] or b[0] > bbox[2] or b[3] < bbox[1] or b[1] > bbox[3]:
                continue
        yield dict(zip(names, sr.record)), shape(sr.shape.__geo_interface__)


def main(raw: Path) -> None:
    lon0 = (FRAME[0] + FRAME[2]) / 2
    lat0 = (FRAME[1] + FRAME[3]) / 2
    k = math.cos(math.radians(lat0))
    scale = WIDTH / ((FRAME[2] - FRAME[0]) * k)
    height = round((FRAME[3] - FRAME[1]) * scale)

    def xy(lon: float, lat: float) -> tuple[float, float]:
        return ((lon - FRAME[0]) * k * scale, (FRAME[3] - lat) * scale)

    def path(geom, closed: bool) -> str:
        parts = []
        geoms = getattr(geom, "geoms", [geom])
        for g in geoms:
            rings = [g.exterior, *g.interiors] if isinstance(g, Polygon) else [g]
            for ring in rings:
                pts = [xy(x, y) for x, y in ring.coords]
                if len(pts) < 2:
                    continue
                d = "M" + "L".join(f"{x:.1f},{y:.1f}" for x, y in pts)
                parts.append(d + ("Z" if closed else ""))
        return "".join(parts)

    frame = box(*FRAME)
    # Tolerância de simplificação em graus: ~1 px no mapa.
    tol = 1 / scale

    # ---- bacia ----
    basins = {d["HYBAS_ID"]: (d, g) for d, g in read(raw / "basins/hybas_sa_lev07_v1c.shp", FRAME)}
    groups = {}
    area = 0.0
    for outlet, name in OUTLETS.items():
        members = {outlet}
        grew = True
        while grew:
            grew = False
            for hid, (d, _) in basins.items():
                if d["NEXT_DOWN"] in members and hid not in members:
                    members.add(hid)
                    grew = True
        groups[name] = unary_union([basins[h][1] for h in members]).buffer(0)
        sub = sum(basins[h][0]["SUB_AREA"] for h in members)
        area += sub
        print(f"{name}: {len(members)} sub-bacias, {sub:,.0f} km²")
    basin = unary_union(list(groups.values())).buffer(0.002).buffer(-0.002)
    print(f"bacia: {area:,.0f} km²")

    # ---- rios ----
    rivers: dict[int, list] = {1: [], 2: [], 3: []}
    near = basin.buffer(0.08)
    for d, g in read(raw / "rivers/HydroRIVERS_v10_sa_shp/HydroRIVERS_v10_sa.shp", FRAME):
        up = d["UPLAND_SKM"]
        if up < MIN_UPLAND or not g.intersects(near):
            continue
        cls = 3 if up >= 20_000 else 2 if up >= 5_000 else 1
        rivers[cls].append(g)
    river_paths = {}
    for cls, segs in rivers.items():
        merged = linemerge(MultiLineString([ls for s in segs for ls in getattr(s, "geoms", [s])]))
        river_paths[cls] = path(merged.simplify(tol), closed=False)

    # ---- contexto: terra (estados sem os lagos; o que sobra é água: Guaíba, Lagoa dos Patos, mar) ----
    states = [(d, g) for d, g in read(raw / "ne/ne_10m_admin_1_states_provinces_lakes.shp", FRAME) if g.intersects(frame)]
    land = unary_union([g for _, g in states]).intersection(frame)
    rs = next(g for d, g in states if d.get("iso_3166_2") == "BR-RS")
    # Divisa do RS só onde ela é fronteira terrestre (não o litoral).
    rs_border = rs.boundary.intersection(frame).intersection(unary_union([g for d, g in states if d.get("iso_3166_2") != "BR-RS"]).buffer(0.01))

    out = {
        "source": "HydroSHEDS (HydroBASINS, HydroRIVERS) · Natural Earth",
        "projection": {"frame": FRAME, "k": k, "scale": scale},
        "width": WIDTH,
        "height": height,
        "basin": path(basin.simplify(tol), closed=True),
        "groups": {n: path(g.simplify(tol * 2).boundary, closed=False) for n, g in groups.items()},
        "rivers": river_paths,
        "land": path(land.simplify(tol), closed=True),
        "state": path(rs_border.simplify(tol), closed=False),
        "labels": [{"name": n, "x": round(xy(lo, la)[0], 1), "y": round(xy(lo, la)[1], 1)} for n, lo, la in LABELS],
    }
    OUT.write_text(json.dumps(out, separators=(",", ":"), ensure_ascii=False))
    print(f"{OUT.relative_to(ROOT)}: {WIDTH}×{height}, {OUT.stat().st_size / 1024:.0f} KiB")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(Path(sys.argv[1]))
