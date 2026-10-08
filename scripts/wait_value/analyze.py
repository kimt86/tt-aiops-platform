#!/usr/bin/env python3
"""기다림의 값어치 — 빈 트럭을 바로 붙이기 vs W초 모아 한꺼번에 붙이기 (2026-10-08).

입력: scripts/wait_value/pairs.sql 의 출력(TSV). 한 줄 = (트럭 자유 → 그 트럭의 다음 픽업) 쌍.
방법: 자유 시각으로 W초 칸을 나누고, 칸 안에서 **그 트럭들이 실제로 한 다음 작업들**을 트럭들에게 다시 나눠
      빈 차 거리 합이 가장 작게 짝짓는다(헝가리안). 실제(TOS 가 한 것)와의 차이 = 모아서 얻는 거리.
      대가 = 칸 끝까지 기다리는 시간(트럭마다 칸 끝 − 자유 시각).
한계: 거리 = 직선(haversine). 작업이 칸 시점에 이미 나와 있었다고 가정하므로 이득은 **상한 쪽**이다.
      W=0(트럭 하나씩)은 구조상 이득 0 — 잣대 확인용.
사용: python3 analyze.py pairs.tsv [경계 epoch(반창 나눔)]
"""
from __future__ import annotations

import math
import random
import statistics
import sys

SPEED_MPS = 8.5655 / 3.6  # learn_travel_metric 최신 median_speed_kmh(2026-10-08) — 거리→시간 거친 환산


def hav(a, b):
    la1, lo1, la2, lo2 = map(math.radians, (a[0], a[1], b[0], b[1]))
    h = math.sin((la2 - la1) / 2) ** 2 + math.cos(la1) * math.cos(la2) * math.sin((lo2 - lo1) / 2) ** 2
    return 2 * 6371000 * math.asin(math.sqrt(h))


def hungarian(cost):
    """정방 행렬 최소 비용 배정. 반환 = 행 i 에 배정된 열."""
    n = len(cost)
    INF = float("inf")
    u = [0.0] * (n + 1)
    v = [0.0] * (n + 1)
    p = [0] * (n + 1)
    way = [0] * (n + 1)
    for i in range(1, n + 1):
        p[0] = i
        j0 = 0
        minv = [INF] * (n + 1)
        used = [False] * (n + 1)
        while True:
            used[j0] = True
            i0 = p[j0]
            delta = INF
            j1 = 0
            for j in range(1, n + 1):
                if not used[j]:
                    cur = cost[i0 - 1][j - 1] - u[i0] - v[j]
                    if cur < minv[j]:
                        minv[j] = cur
                        way[j] = j0
                    if minv[j] < delta:
                        delta = minv[j]
                        j1 = j
            for j in range(n + 1):
                if used[j]:
                    u[p[j]] += delta
                    v[j] -= delta
                else:
                    minv[j] -= delta
            j0 = j1
            if p[j0] == 0:
                break
        while True:
            j1 = way[j0]
            p[j0] = p[j1]
            j0 = j1
            if j0 == 0:
                break
    ans = [0] * n
    for j in range(1, n + 1):
        if p[j]:
            ans[p[j] - 1] = j - 1
    return ans


def load(path):
    rows = []
    for line in open(path):
        c = line.rstrip("\n").split("\t")
        if len(c) < 9 or "" in c[5:9]:
            continue
        rows.append(dict(ytno=c[0], f=int(c[1]), fjt=c[2], p=int(c[3]), pjt=c[4],
                         a=(float(c[5]), float(c[6])), b=(float(c[7]), float(c[8]))))
    return rows


def run(rows, W, rng=None):
    """반환 = (n, 실제 합 m, 최적 합 m, 위약(무작위) 합 m, 평균 대기 s)."""
    buckets = {}
    for r in rows:
        k = r["f"] if W == 0 else r["f"] // W
        buckets.setdefault((k, r["ytno"]) if W == 0 else k, []).append(r)
    act = opt = plc = wait = 0.0
    n = 0
    for k, rs in buckets.items():
        m = len(rs)
        n += m
        a = sum(hav(r["a"], r["b"]) for r in rs)
        act += a
        if W == 0 or m == 1:
            opt += a
            plc += a
        else:
            cost = [[hav(ri["a"], rj["b"]) for rj in rs] for ri in rs]
            asg = hungarian(cost)
            opt += sum(cost[i][asg[i]] for i in range(m))
            perm = list(range(m))
            (rng or random).shuffle(perm)
            plc += sum(cost[i][perm[i]] for i in range(m))
            end = (k + 1) * W
            wait += sum(end - r["f"] for r in rs)
    return n, act, opt, plc, wait / max(n, 1)


def report(rows, title):
    print(f"\n== {title} — 쌍 {len(rows):,}개 (분모: 자유→다음 픽업 30분 안·좌표 있는 쌍)")
    print(f"{'W(초)':>6} {'트럭/칸':>7} {'실제 km':>9} {'최적 km':>9} {'아낀 %':>7} {'트럭당 아낀 m':>12} "
          f"{'≈초':>6} {'트럭당 대기 s':>12} {'위약(무작위) km':>14} {'6초 대비 아낀 s':>14} {'추가 대기 s':>10} {'순 s':>7}")
    rng = random.Random(7)
    base = None  # W=6 = TOS 배차 주기(~5.5초) 근사 — 같은 잣대(직선·같은 풀이)로 비교하기 위한 기준
    for W in (0, 6, 60, 180, 300):
        n, act, opt, plc, wait = run(rows, W, rng)
        per_m = (act - opt) / n
        nb = len({(r['f'] // W) for r in rows}) if W else n
        if W == 6:
            base = (opt, wait)
        rel = ""
        if base and W > 6:
            save_s = (base[0] - opt) / n / SPEED_MPS
            extra = wait - base[1]
            rel = f"{save_s:>14.0f} {extra:>10.0f} {save_s - extra:>7.0f}"
        print(f"{W:>6} {n / nb:>7.1f} {act / 1000:>9.1f} {opt / 1000:>9.1f} {100 * (act - opt) / act:>7.1f} "
              f"{per_m:>12.1f} {per_m / SPEED_MPS:>6.0f} {wait:>12.1f} {plc / 1000:>14.1f} {rel}")


def main():
    rows = load(sys.argv[1])
    report(rows, "전체")
    if len(sys.argv) > 2:
        cut = int(sys.argv[2])
        report([r for r in rows if r["f"] < cut], "앞 반창")
        report([r for r in rows if r["f"] >= cut], "뒤 반창")
    d = [hav(r["a"], r["b"]) for r in rows]
    print(f"\n실제 빈 차 직선거리 중앙 {statistics.median(d):.0f} m · 평균 {statistics.mean(d):.0f} m")


if __name__ == "__main__":
    main()
