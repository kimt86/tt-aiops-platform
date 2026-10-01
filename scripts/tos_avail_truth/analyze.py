"""tos_avail_interval 을 2초 간격 실측(run.sh 출력)과 대조한다.

사용: analyze.py <출력폴더> <비교 시작 순간 UTC ISO>  — 그 순간 이후 표본만 "fixed-bin" 줄에 잡힌다(배포 경계 가르기용).
분모 = (트럭, 표본 순간) 쌍. 짧은 머묾 절 = 우리 구간 안에 떨어진 표본에서 그 트럭이 실제 목록에 있었나(구간이 진짜인가).
"""
import json, sys, subprocess, collections, bisect
from datetime import datetime as dt, timedelta as td, timezone
D = sys.argv[1]; FIX = dt.fromisoformat(sys.argv[2])  # first instant replayed by the fixed binary (UTC)
UTC = timezone.utc
def P(s):  # MYT 'YYYYMMDDHH24MISSFF3' -> UTC
    b = dt.strptime(s[:14], "%Y%m%d%H%M%S") - td(hours=8)
    return (b + td(milliseconds=int(s[14:17]))).replace(tzinfo=UTC)
def load(path):
    out = []
    for line in open(path):
        line = line.strip()
        if not line: continue
        try: r = json.loads(json.loads(line)["result"] or "[]")
        except Exception: continue
        mark = [x for x in r if x["ID"] == "MARK"]
        if mark: out.append((P(mark[0]["TS"]), {x["ID"] for x in r if x["ID"] != "MARK"}))
    return sorted(out, key=lambda x: x[0])
truth = load(f"{D}/truth.jsonl"); stops = load(f"{D}/stops.jsonl")
st_t = [t for t, _ in stops]
def stopped_at(t):
    i = bisect.bisect_right(st_t, t) - 1
    return stops[i][1] if i >= 0 else set()
truth = [(t, s - stopped_at(t)) for t, s in truth]
q = lambda sql: subprocess.run(["psql", "-h", "127.0.0.1", "-p", "5433", "-U", "wp", "wp_tt", "-AtF", "\t", "-c", sql],
                               capture_output=True, text=True, env={"PGPASSWORD": "wp", "PATH": "/usr/bin:/bin"}).stdout
anchor = dt.fromisoformat(q("select to_char(as_of_ts at time zone 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US') from tos_avail_anchor").strip()).replace(tzinfo=UTC)
t0, t1 = truth[0][0], truth[-1][0]
rows = q(f"select ytno, to_char(enter_ts at time zone 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US'), coalesce(to_char(exit_ts at time zone 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US'),''), enter_cause, coalesce(exit_cause,''), coalesce(enter_program,''), coalesce(exit_program,'') from tos_avail_interval where enter_ts <= '{t1.isoformat()}' and (exit_ts is null or exit_ts > '{t0.isoformat()}')")
iv = []
for line in rows.splitlines():
    y, en, ex, ec, xc, ep, xp = line.split("\t")
    en = dt.fromisoformat(en).replace(tzinfo=UTC); ex = dt.fromisoformat(ex).replace(tzinfo=UTC) if ex else None
    iv.append(dict(y=y, en=en, ex=ex, ec=ec, xc=xc, ep=ep, xp=xp))
by_truck = collections.defaultdict(list)
for i in iv: by_truck[i["y"]].append(i)
def ours(t):
    return {i["y"] for i in iv if i["en"] <= t and (i["ex"] is None or t < i["ex"])}
truth = [(t, s) for t, s in truth if t <= anchor]
gaps = sorted((b[0] - a[0]).total_seconds() for a, b in zip(truth, truth[1:])); dlt = sum(gaps) / len(gaps)
print(f"truth samples {len(truth)}  {t0+td(hours=8):%H:%M:%S}..{truth[-1][0]+td(hours=8):%H:%M:%S} MYT  interval mean {dlt:.2f}s  anchor {anchor+td(hours=8):%H:%M:%S}")
print("truth list size med", sorted(len(s) for _, s in truth)[len(truth)//2], "max", max(len(s) for _, s in truth), " stops seen", set().union(*[s for _, s in stops]) or "none")
def trans_near(y, t, w):
    for i in by_truck.get(y, []):
        for x in (i["en"], i["ex"]):
            if x is not None and abs((x - t).total_seconds()) <= w: return True
    return False
def score(name, samples):
    tp = fp = fn = 0; fpb = fnb = 0; errs = []
    for t, s in samples:
        o = ours(t)
        tp += len(s & o); fp += len(o - s); fn += len(s - o)
        for y in o - s:
            b = trans_near(y, t, 1.0); fpb += b; errs.append(("FP", y, t, b))
        for y in s - o:
            b = trans_near(y, t, 1.0); fnb += b; errs.append(("FN", y, t, b))
    n = tp + fn; m = tp + fp
    print(f"{name:10s} samples {len(samples):4d}  recall {100*tp/n if n else 0:5.1f}% ({tp}/{n})  precision {100*tp/m if m else 0:5.1f}% ({tp}/{m})"
          f"  errors within 1s of that truck's own transition: FP {fpb}/{fp}  FN {fnb}/{fn}")
    return errs
print("\n== (truck, instant) agreement — 분모: 정답 목록에 있던 (트럭, 순간) / 우리가 올린 (트럭, 순간)")
score("all", truth)
score("old-bin", [x for x in truth if x[0] < FIX])
errs = score("fixed-bin", [x for x in truth if x[0] >= FIX])
print("\n== fixed-bin errors not within 1s of a transition (genuine):")
for kind, y, t, b in errs:
    if b: continue
    near = sorted(by_truck.get(y, []), key=lambda i: abs((i["en"] - t).total_seconds()))[:2]
    print(f"  {kind} {y} @{t+td(hours=8):%H:%M:%S.%f}"[:-3], [(f"{i['en']+td(hours=8):%H:%M:%S}", i['ec'], i['ep'][:25], f"{i['ex']+td(hours=8):%H:%M:%S}" if i['ex'] else None, i['xc']) for i in near])
print("\n== short stays: are they real? (fixed-bin, intervals fully inside the sampled window)")
fixed_iv = [i for i in iv if i["ex"] is not None and i["en"] >= FIX and i["ex"] <= truth[-1][0] and i["ec"] not in ("init", "reconcile")]
tt = [t for t, _ in truth]; tset = dict(truth)
for lo, hi in ((0, 1), (1, 2), (2, 5), (5, 1e9)):
    sel = [i for i in fixed_iv if lo <= (i["ex"] - i["en"]).total_seconds() < hi]
    exp = sum(min((i["ex"] - i["en"]).total_seconds(), dlt) / dlt for i in sel)
    inside = conf = 0
    for i in sel:
        a = bisect.bisect_left(tt, i["en"]); b = bisect.bisect_left(tt, i["ex"])
        for k in range(a, b):
            inside += 1; conf += i["y"] in truth[k][1]
    print(f"  {lo}-{hi if hi < 1e9 else '∞'}s: intervals {len(sel):4d}  samples falling inside {inside:4d} (expected if sampling uniform ≈{exp:.0f})  truck in truth at those samples {conf}/{inside}"
          + (f" = {100*conf/inside:.0f}%" if inside else ""))
print("\n== logout-detach stays (enter_program 'Event : USR_LOGIN_INFO'):")
sel = [i for i in iv if i["ep"].startswith("Event : USR_LOGIN_INFO") and i["ex"] is not None and i["en"] >= t0 and i["ex"] <= truth[-1][0]]
inside = conf = 0
for i in sel:
    a = bisect.bisect_left(tt, i["en"]); b = bisect.bisect_left(tt, i["ex"])
    for k in range(a, b): inside += 1; conf += i["y"] in truth[k][1]
print(f"  intervals {len(sel)}  exit causes {collections.Counter(i['xc'] for i in sel)}  samples inside {inside}  truck in truth {conf}/{inside}")
print("\n== truth runs (truck present in consecutive samples) covered by our intervals (fixed-bin):")
runs = []; cur = {}
for k, (t, s) in enumerate(truth):
    for y in list(cur):
        if y not in s: runs.append((y, cur.pop(y), k - 1))
    for y in s: cur.setdefault(y, k)
cov = collections.Counter()
for y, a, b in runs:
    if truth[a][0] < FIX: continue
    hit = any(y in ours(truth[k][0]) for k in range(a, b + 1))
    cov[(b - a + 1 >= 2, hit)] += 1
print(f"  runs of 1 sample: covered {cov[(False, True)]}/{cov[(False, True)] + cov[(False, False)]}   runs of 2+ samples: covered {cov[(True, True)]}/{cov[(True, True)] + cov[(True, False)]}")
