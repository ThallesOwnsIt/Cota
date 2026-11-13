#!/usr/bin/env python3
"""Health check of Cota's live mode. Read-only: it never writes anything, on-chain or off.

    python3 scripts/doctor.py [--rpc URL] [--json]

Checks:
  lote      the chain is not late: a closed UTC day still waiting for its batch LATE_HOURS after it closed
  árvore    the local ledger has every record the chain has, and the latest audit found no problem
  saldo     the operator (who pays the fees) has SOL left
  fontes    ANA, INMET, Open-Meteo, the Solana RPC and GitHub answer
  carimbo   the accumulator's root was stamped in Bitcoin in the last days
  ledger    the ledger was committed recently, and every local commit reached GitHub
  disco     free space where the checkout lives

After the cutover (COTA_LEDGER=v3) the chain checks give way to the v3 ledger's (V3_LEDGER, default data/ledger-v3):
  livro     the ledger replays in full with cota-ledger3 verify (rules, signatures, checkpoints, readings, contents,
            the opening committee against v2's on-chain)
  lote      no closed day waits for its batch more than LATE_HOURS
  ponto     the latest checkpoint has its Bitcoin proof, confirmed within a day
and there is no SOL to check. During the rehearsal (COTA_V3_ENSAIO=1) `ensaio` replays data/ledger-v3-ensaio and only
warns.

Exit code: 0 all fine, 1 warnings, 2 failures. Runs with the standard library only (host, container, laptop).
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import shutil
import struct
import subprocess
import time
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LEDGER = ROOT / "data/ledger"
# PDA ["registry"] of program v2 FnjR7wG5zicqWJ6KQuWQ5UcqqhcSghrYozVgMmmDFNwR.
REGISTRY = "6CNtD18LQ4bBZTGK1C2Z6RuJrSwyxCD3ToRpiWE3y9bR"
# The registry authority (keys/operator.json), which pays the live mode's fees.
OPERATOR = "3wPjiZdtuJpmivtLCgqgL7gyeSTrbydj7U7R7wwHCe2v"
UA = {"User-Agent": "cota-doctor"}
V3_MODE = os.environ.get("COTA_LEDGER", "v2") == "v3"
V3_LEDGER = ROOT / os.environ.get("V3_LEDGER", "data/ledger-v3")
ENSAIO = ROOT / "data/ledger-v3-ensaio"
BIN = Path(os.environ.get("BIN", ROOT / "target/release"))
# OpenTimestamps attestation tag of a Bitcoin block header (a proof is confirmed once it holds one).
OTS_BITCOIN = bytes.fromhex("0588960d73d71901")

OK, WARN, FAIL = "ok", "aviso", "falha"
SYMBOL = {OK: "✓", WARN: "!", FAIL: "✗"}


def rpc(url: str, method: str, params: list) -> dict:
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json", **UA})
    with urllib.request.urlopen(req, timeout=30) as r:
        reply = json.load(r)
    if "error" in reply:
        raise RuntimeError(reply["error"].get("message", reply["error"]))
    return reply["result"]


def registry(url: str) -> dict:
    data = base64.b64decode(
        rpc(url, "getAccountInfo", [REGISTRY, {"encoding": "base64", "commitment": "confirmed"}])["value"]["data"][0]
    )
    # Registry: discriminator (8) | authority (32) | members: u32 + 32·n | threshold u8 | next_batch_id u64 |
    # last_batch_root (32) | last_t_end i64 | leaf_count u64 | peaks | bump
    o = 8 + 32
    n = struct.unpack_from("<I", data, o)[0]
    o += 4 + 32 * n
    threshold = data[o]
    o += 1
    next_batch_id = struct.unpack_from("<Q", data, o)[0]
    o += 8 + 32
    last_t_end, leaf_count = struct.unpack_from("<qQ", data, o)
    return {"members": n, "threshold": threshold, "next_batch_id": next_batch_id, "last_t_end": last_t_end, "leaf_count": leaf_count}


def reachable(url: str) -> tuple[bool, str]:
    """Any HTTP answer below 500 counts: the service is up, even if it wants a login."""
    try:
        with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=20) as r:
            return True, f"HTTP {r.status}"
    except urllib.error.HTTPError as e:
        return e.code < 500, f"HTTP {e.code}"
    except Exception as e:  # timeouts, DNS, TLS
        return False, type(e).__name__


def ago(seconds: float) -> str:
    h = seconds / 3600
    return f"{h:.0f} h" if h >= 1 else f"{seconds / 60:.0f} min"


def check_batch(reg: dict, late_hours: float) -> tuple[str, str]:
    now = datetime.now(timezone.utc)
    start = datetime.fromtimestamp(reg["last_t_end"], timezone.utc)
    last_day = (start - timedelta(days=1)).date()
    if start.date() >= now.date():
        return OK, f"em dia: último lote {last_day} (nº {reg['next_batch_id'] - 1})"
    # The day starting at last_t_end has closed and is still waiting for its batch.
    late = (now - (start + timedelta(days=1))).total_seconds()
    msg = f"{start.date()} fechou há {ago(late)} e ainda não tem lote (último: {last_day})"
    return (FAIL, msg) if late > late_hours * 3600 else (OK, msg + "; dentro da janela")


def check_tree(reg: dict) -> tuple[str, str]:
    path = LEDGER / "records.jsonl"
    local = sum(1 for line in path.read_text().splitlines() if line.strip()) if path.exists() else 0
    chain = reg["leaf_count"]
    problems = []
    audit_path = LEDGER / "audit.json"
    if audit_path.exists():
        audit = json.loads(audit_path.read_text())
        acc, summary = audit.get("accumulator", {}), audit.get("summary", {})
        if not acc.get("peaks_match", False):
            problems.append("a última auditoria não bateu os picos da árvore com a cadeia")
        bad = {k: v for k, v in summary.items() if k.endswith("_with_problems") and v}
        if bad:
            problems.append("auditoria com problemas: " + ", ".join(f"{k}={v}" for k, v in bad.items()))
        if summary.get("unmatched"):
            problems.append(f"{summary['unmatched']} registro(s) sem arquivo correspondente no ledger")
    else:
        problems.append("sem data/ledger/audit.json")
    if problems:
        return FAIL, "; ".join(problems)
    if local < chain:
        return WARN, f"ledger local tem {local} registros e a cadeia {chain} (falta git pull, ou uma rodada não publicou)"
    if local > chain:
        return FAIL, f"ledger local tem {local} registros, mais que a cadeia ({chain})"
    return OK, f"{chain} registros, iguais à cadeia; última auditoria sem problemas"


def check_balance(url: str, operator: str, daily: float) -> tuple[str, str]:
    sol = rpc(url, "getBalance", [operator, {"commitment": "confirmed"}])["value"] / 1e9
    days = sol / daily
    msg = f"operador {operator[:8]}… tem {sol:.3f} SOL, ≈ {days:.0f} dias no ritmo de {daily} SOL/dia"
    if days < 7:
        return FAIL, msg + " (recarregar já)"
    if days < 30:
        return WARN, msg + " (recarregar em breve)"
    return OK, msg


def check_sources(url: str) -> tuple[str, str]:
    targets = {
        "ANA": "https://www.ana.gov.br/hidrowebservice/EstacoesTelemetricas/OAUth/v1",
        "INMET": "https://apiprevmet3.inmet.gov.br/avisos/ativos",
        "GloFAS": "https://flood-api.open-meteo.com/v1/flood?latitude=-30.03&longitude=-51.23&daily=river_discharge&forecast_days=1",
        "chuva": "https://single-runs-api.open-meteo.com/v1/forecast?latitude=-30.03&longitude=-51.23&hourly=precipitation",
        "GitHub": "https://github.com/ThallesOwnsIt/Cota",
    }
    down = []
    for name, target in targets.items():
        up, detail = reachable(target)
        if not up:
            down.append(f"{name} ({detail})")
    try:
        health = rpc(url, "getHealth", [])
        if health != "ok":
            down.append(f"RPC ({health})")
    except Exception as e:
        down.append(f"RPC ({type(e).__name__})")
    if down:
        return WARN, "sem resposta: " + ", ".join(down)
    return OK, "ANA, INMET, GloFAS, chuva prevista, RPC e GitHub respondem"


def check_stamp() -> tuple[str, str]:
    path = LEDGER / "timestamps/index.json"
    if not path.exists():
        return WARN, "nenhum carimbo ainda"
    stamps = json.loads(path.read_text())
    last = stamps[-1]
    age = (datetime.now(timezone.utc) - datetime.fromisoformat(last["stamped"])).total_seconds()
    pending = [s for s in stamps if not s.get("bitcoin_block")]
    msg = f"último carimbo há {ago(age)} ({last['records']} registros"
    msg += f", bloco Bitcoin {last['bitcoin_block']})" if last.get("bitcoin_block") else ", aguardando bloco Bitcoin)"
    if age > 36 * 3600:
        return WARN, msg + "; deveria ser diário"
    if any((datetime.now(timezone.utc) - datetime.fromisoformat(s["stamped"])).total_seconds() > 24 * 3600 for s in pending):
        return WARN, msg + "; há carimbo esperando o Bitcoin há mais de 24 h"
    return OK, msg


def ledger3(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run([str(BIN / "cota-ledger3"), *args], capture_output=True, text=True, timeout=600, cwd=ROOT)


def check_v3_ledger(path: Path, url: str, warn_only: bool = False) -> tuple[str, str]:
    if not (path / "entries.jsonl").exists():
        return WARN, f"{path.relative_to(ROOT)} ainda não tem entradas"
    r = ledger3("--ledger", str(path), "verify", "--rpc", url)
    out = {l.split(None, 1)[0]: l.split(None, 1)[1] for l in r.stdout.splitlines() if len(l.split(None, 1)) == 2}
    if r.returncode != 0:
        return (WARN if warn_only else FAIL), "não confere: " + " ".join((r.stderr or r.stdout).split())[-300:]
    return OK, f"confere: {out.get('entradas', '?')}; {out.get('pontos', '?')}; relógio {out.get('relógio', '?')}"


def check_v3_batch(path: Path, late_hours: float) -> tuple[str, str]:
    r = ledger3("--ledger", str(path), "next-day", "--settle-min", "0")
    if r.returncode != 0:
        return FAIL, "não consegui consultar o livro: " + " ".join(r.stderr.split())[-200:]
    if not r.stdout.strip():
        return OK, "em dia: nenhum dia fechado à espera de lote"
    day = datetime.fromisoformat(r.stdout.split()[0]).replace(tzinfo=timezone.utc).date()
    closed = datetime(day.year, day.month, day.day, tzinfo=timezone.utc) + timedelta(days=1)
    late = (datetime.now(timezone.utc) - closed).total_seconds()
    msg = f"{day} fechou há {ago(late)} e ainda não tem lote"
    return (FAIL, msg) if late > late_hours * 3600 else (OK, msg + "; dentro da janela")


def check_v3_checkpoint(path: Path) -> tuple[str, str]:
    files = sorted((path / "checkpoints").glob("*.json"))
    if not files:
        return WARN, "nenhum ponto de controle ainda"
    last = files[-1]
    proof = last.with_name(last.name + ".ots")
    age = time.time() - last.stat().st_mtime
    msg = f"último ponto de controle {last.stem.lstrip('0')} (há {ago(age)})"
    if not proof.exists():
        return (WARN, msg + ", sem carimbo no Bitcoin") if age > 3600 else (OK, msg + ", carimbo a caminho")
    if OTS_BITCOIN in proof.read_bytes():
        return OK, msg + ", carimbo confirmado no Bitcoin"
    return (WARN, msg + ", carimbo esperando o Bitcoin há mais de 24 h") if age > 24 * 3600 else (OK, msg + ", carimbo enviado, aguardando bloco")


def check_commit(path: str = "data/ledger") -> tuple[str, str]:
    try:
        out = subprocess.run(
            ["git", "-C", str(ROOT), "log", "-1", "--format=%ct %s", "--", path],
            capture_output=True, text=True, timeout=30, check=True,
        ).stdout.strip()
    except Exception as e:
        return WARN, f"git indisponível ({type(e).__name__})"
    if not out:
        return WARN, "nenhum commit do ledger"
    ts, subject = out.split(" ", 1)
    age = time.time() - int(ts)
    msg = f"último commit do ledger há {ago(age)} ({subject})"
    # Commits made here that GitHub does not have yet (the push failed): the site and every other copy fall
    # behind while the round still looks healthy locally.
    try:
        unpushed = subprocess.run(
            ["git", "-C", str(ROOT), "log", "--reverse", "--format=%ct", "@{u}..HEAD", "--", path],
            capture_output=True, text=True, timeout=30, check=True,
        ).stdout.split()
    except Exception:
        unpushed = []  # no upstream (a laptop checkout, say): nothing to compare with
    if unpushed:
        waiting = time.time() - int(unpushed[0])
        late = f"{len(unpushed)} commit(s) do ledger ainda não enviados ao GitHub, o mais antigo há {ago(waiting)}"
        return (FAIL if waiting > 3600 else WARN), f"{msg}; {late}"
    return (WARN, msg + "; esperado ao menos um por dia") if age > 26 * 3600 else (OK, msg)


def check_disk() -> tuple[str, str]:
    free = shutil.disk_usage(ROOT).free / 1e9
    msg = f"{free:.1f} GB livres"
    if free < 0.5:
        return FAIL, msg
    return (WARN, msg) if free < 2 else (OK, msg)


def main() -> None:
    ap = argparse.ArgumentParser(description="Diagnóstico do modo ao vivo do Cota (só leitura).")
    ap.add_argument("--rpc", default=os.environ.get("RPC", "https://api.devnet.solana.com"))
    ap.add_argument("--operator", default=OPERATOR)
    # Measured on 2026-11-11: 200 transactions in 7.4 days cost 0.0407 SOL (one batch, outlook and forecast a day,
    # official warnings, occasional degradation reports and vault payouts).
    ap.add_argument("--daily-sol", type=float, default=0.0055, help="gasto diário do operador, em SOL")
    ap.add_argument("--late-hours", type=float, default=3, help="horas depois do fechamento do dia sem lote = falha")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    results: list[tuple[str, str, str]] = []

    def run(name: str, fn, *a) -> None:
        try:
            status, detail = fn(*a)
        except Exception as e:
            status, detail = FAIL, f"não consegui verificar: {type(e).__name__}: {e}"
        results.append((name, status, detail))

    if V3_MODE:
        # After the cutover: the v3 ledger; v2 is a frozen archive and nothing pays fees.
        run("livro", check_v3_ledger, V3_LEDGER, args.rpc)
        run("lote", check_v3_batch, V3_LEDGER, args.late_hours)
        run("ponto", check_v3_checkpoint, V3_LEDGER)
        run("fontes", check_sources, args.rpc)
        run("ledger", check_commit, str(V3_LEDGER.relative_to(ROOT)))
        run("disco", check_disk)
    else:
        try:
            reg = registry(args.rpc)
        except Exception as e:
            reg = None
            results.append(("cadeia", FAIL, f"não consegui ler o registro na cadeia: {type(e).__name__}: {e}"))
        if reg:
            run("lote", check_batch, reg, args.late_hours)
            run("árvore", check_tree, reg)
        run("saldo", check_balance, args.rpc, args.operator, args.daily_sol)
        run("fontes", check_sources, args.rpc)
        run("carimbo", check_stamp)
        run("ledger", check_commit)
        run("disco", check_disk)
        # The rehearsal: watched, but it never fails the round.
        if os.environ.get("COTA_V3_ENSAIO") == "1" and ENSAIO.exists():
            run("ensaio", check_v3_ledger, ENSAIO, args.rpc, True)

    worst = max((s for _, s, _ in results), key=[OK, WARN, FAIL].index)
    if args.json:
        print(json.dumps({"status": worst, "checks": [{"check": n, "status": s, "detail": d} for n, s, d in results]}, ensure_ascii=False, indent=1))
    else:
        for name, status, detail in results:
            print(f"{SYMBOL[status]} {name:<8} {detail}")
        print({OK: "\ntudo certo", WARN: "\natenção aos avisos acima", FAIL: "\nhá falhas: veja o manual de operação (docs/operations.md, no repositório privado cota-site)"}[worst])
    raise SystemExit([OK, WARN, FAIL].index(worst))


if __name__ == "__main__":
    main()
