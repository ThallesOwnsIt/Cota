"""Métricas de avaliação (seções 6.8 e 6.10 do briefing). Todas O(n)."""

from __future__ import annotations

import numpy as np


def pinball(y: np.ndarray, q: np.ndarray, tau: float) -> float:
    d = y - q
    return float(np.mean(np.maximum(tau * d, (tau - 1) * d)))


def crps_approx(y: np.ndarray, preds: dict[float, np.ndarray]) -> float:
    """CRPS aproximado pela média das perdas pinball × 2 sobre os quantis."""
    return 2 * float(np.mean([pinball(y, q, tau) for tau, q in preds.items()]))


def coverage(y: np.ndarray, lo: np.ndarray, hi: np.ndarray) -> float:
    return float(np.mean((y >= lo) & (y <= hi)))


def nse(obs: np.ndarray, sim: np.ndarray) -> float:
    return float(1 - np.sum((obs - sim) ** 2) / np.sum((obs - obs.mean()) ** 2))


def kge(obs: np.ndarray, sim: np.ndarray) -> float:
    r = np.corrcoef(obs, sim)[0, 1]
    alpha = sim.std() / obs.std()
    beta = sim.mean() / obs.mean()
    return float(1 - np.sqrt((r - 1) ** 2 + (alpha - 1) ** 2 + (beta - 1) ** 2))


def contingency(alert: np.ndarray, event: np.ndarray) -> dict[str, float | int]:
    """Acerto a, alarme falso b, falha c → POD, FAR, CSI."""
    a = int(np.sum(alert & event))
    b = int(np.sum(alert & ~event))
    c = int(np.sum(~alert & event))
    nan = float("nan")
    return {
        "hits": a,
        "false_alarms": b,
        "misses": c,
        "pod": a / (a + c) if a + c else nan,
        "far": b / (a + b) if a + b else nan,
        "csi": a / (a + b + c) if a + b + c else nan,
    }
