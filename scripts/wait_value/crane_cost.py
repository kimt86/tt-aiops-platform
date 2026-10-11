#!/usr/bin/env python3
"""모아 붙이기의 크레인 쪽 손해 — 트럭이 늦게 닿아 안벽 크레인이 기다리게 되는 시간 (2026-10-12).

입력: scripts/wait_value/pairs.sql 의 출력(TSV, 11열). analyze.py 와 같은 쌍·같은 풀이(W초 칸 안 헝가리안).
방법: 작업(= 각 쌍의 '다음 픽업') 마다 픽업 도착 시각을 계산한다.
        실제 짝(참조) : 그 작업을 실제로 한 트럭이 빈 순간 바로 출발 = 자유 시각 + 직선거리/속도
        W초 칸        : 칸 끝 + (배정된 트럭 → 그 작업 직선거리)/속도
      Δ = W초 도착 − 실제 짝 도착. Δ>0 이면 그 작업의 트럭이 늦게 닿는다.
      참조를 실제 짝으로 두는 이유: 여유(10열)는 실제 짝에서 잰 값이다. 6초 칸을 참조로 쓰면 6초 칸
      자체가 짝을 섞어 생긴 차이까지 손해로 잡힌다(1초 칸 대 6초 칸만으로 24초/작업이 나왔다).
      그래서 W초의 손해에서 **6초 칸의 손해(= 이 잣대의 바닥값)를 뺀 증분**을 analyze.py 의 '6초 대비'와 짝지어 본다.
      적하는 픽업(야드)에서 늦은 만큼 안벽에도 늦게 닿는다고 본다(싣고 가는 길은 같은 상자라 같다 —
      야드 크레인 앞 대기가 흡수해 줄 몫은 안 뺐다 = 손해 쪽으로 과장).
      크레인 손해 = max(0, Δ − 여유). 여유 = 실제로 트럭이 크레인 앞에서 기다린 시간(10열) − 크레인이
      트럭을 다루는 데 드는 최소 시간(h). h 를 빼지 않으면 다루는 시간까지 여유로 쳐서 손해를 덜 센다.
세 가지 값:
  상한        — 여유를 전혀 안 믿음: max(0, Δ) (여유를 모르는 작업까지 전부)
  여유 반영   — h = 유형별 여유 하위 10% 값, 도착 출처 전부
  정밀 출처만 — 같은 식, 도착 출처가 arr_dtime(피드가 준 도착 시각)인 작업만
한계: 크레인 손해가 다음 상자로 번지는 효과(연쇄)·트럭이 일찍 닿아 크레인이 덜 기다리는 이득은 안 셌다.
사용: python3 crane_cost.py pairs.tsv [경계 epoch(반창 나눔)]
"""
from __future__ import annotations

import statistics
import sys

from analyze import SPEED_MPS, hav, hungarian

WINDOWS = (6, 60, 180, 300)  # 6 = 바닥값(TOS 배차 주기 근사)


def load(path):
    rows = []
    for line in open(path):
        c = line.rstrip("\n").split("\t")
        if len(c) < 9 or "" in c[5:9]:
            continue
        slack = int(c[9]) if len(c) > 9 and c[9] != "" else None
        src = c[10] if len(c) > 10 and c[10] != "" else None
        rows.append(dict(ytno=c[0], f=int(c[1]), fjt=c[2], p=int(c[3]), pjt=c[4],
                         a=(float(c[5]), float(c[6])), b=(float(c[7]), float(c[8])),
                         slack=slack, src=src))
    return rows


def arrivals(rows, W):
    """작업(행 번호) → W초 칸에서의 픽업 도착 시각(epoch 초). 칸 안에서 analyze.run 과 같은 풀이.
    W=0 = 실제 짝(그 트럭이 자기 작업으로 자유 시각에 바로 출발)."""
    if W == 0:
        return [r["f"] + hav(r["a"], r["b"]) / SPEED_MPS for r in rows]
    buckets = {}
    for i, r in enumerate(rows):
        buckets.setdefault(r["f"] // W, []).append(i)
    arr = [0.0] * len(rows)
    for k, idx in buckets.items():
        end = (k + 1) * W
        if len(idx) == 1:
            i = idx[0]
            arr[i] = end + hav(rows[i]["a"], rows[i]["b"]) / SPEED_MPS
            continue
        cost = [[hav(rows[ti]["a"], rows[tj]["b"]) for tj in idx] for ti in idx]
        asg = hungarian(cost)
        for t, j in enumerate(asg):
            arr[idx[j]] = end + cost[t][j] / SPEED_MPS
    return arr


def floor_h(rows):
    """유형별 h = 여유 하위 10% (크레인이 트럭을 다루는 최소 시간의 근사)."""
    h = {}
    for jt in ("DS", "LD"):
        v = sorted(r["slack"] for r in rows if r["pjt"] == jt and r["slack"] is not None)
        h[jt] = statistics.quantiles(v, n=10)[0] if len(v) >= 10 else 0.0
    return h


def report(rows, title, h):
    n = len(rows)
    with_slack = [i for i, r in enumerate(rows) if r["slack"] is not None]
    precise = [i for i in with_slack if rows[i]["src"] == "arr_dtime"]
    print(f"\n== {title} — 작업 {n:,}개 · 여유 아는 작업 {len(with_slack):,} ({100 * len(with_slack) / max(n, 1):.0f}%)"
          f" · 정밀 출처 {len(precise):,} · h DS {h['DS']:.0f}s / LD {h['LD']:.0f}s")
    print(f"{'W(초)':>6} {'Δ평균 s':>8} {'늦어진 작업 %':>12} {'늦을 때 Δ중앙 s':>15} "
          f"{'상한 s/작업':>11} {'여유 반영 s/작업':>15} {'정밀만 s/작업':>13} {'손해 생긴 작업 %':>15}"
          f" │ {'6초 대비 증분: 상한':>18} {'여유 반영':>9} {'정밀만':>7}")
    ref = arrivals(rows, 0)

    def loss(d, i):
        s_eff = max(0.0, rows[i]["slack"] - h[rows[i]["pjt"]])
        return max(0.0, d[i] - s_eff)

    base = None
    for W in WINDOWS:
        arr = arrivals(rows, W)
        d = [arr[i] - ref[i] for i in range(n)]
        late = [x for x in d if x > 0]
        upper = sum(max(0.0, x) for x in d) / n
        adj = [loss(d, i) for i in with_slack]
        pre = [loss(d, i) for i in precise]
        m_adj = statistics.mean(adj) if adj else float("nan")
        m_pre = statistics.mean(pre) if pre else float("nan")
        hit = sum(x > 0 for x in adj) / max(len(adj), 1)
        inc = ""
        if W == 6:
            base = (upper, m_adj, m_pre)
        elif base:
            inc = f" │ {upper - base[0]:>18.1f} {m_adj - base[1]:>9.1f} {m_pre - base[2]:>7.1f}"
        print(f"{W:>6} {statistics.mean(d):>8.1f} {100 * len(late) / n:>12.1f} "
              f"{(statistics.median(late) if late else 0):>15.0f} {upper:>11.1f} "
              f"{m_adj:>15.1f} {m_pre:>13.1f} {100 * hit:>15.1f}{inc}")


def main():
    rows = load(sys.argv[1])
    h = floor_h(rows)  # h 는 전체 창에서 한 번 정해 반창에도 같은 값을 쓴다
    report(rows, "전체", h)
    report([r for r in rows if r["pjt"] == "DS"], "다음 작업이 양하(QC 가 실어줌 · 위치 = 크레인 좌표)", h)
    report([r for r in rows if r["pjt"] == "LD"], "다음 작업이 적하(야드 픽업 위치 = GPS)", h)
    if len(sys.argv) > 2:
        cut = int(sys.argv[2])
        report([r for r in rows if r["f"] < cut], "앞 반창", h)
        report([r for r in rows if r["f"] >= cut], "뒤 반창", h)


if __name__ == "__main__":
    main()
