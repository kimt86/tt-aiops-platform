#!/usr/bin/env python
# 이동시간 GBM 그림자 (mig 0089). 배차 미변경 — 검증 전용.
# 매 실행: (1) 모델이 없거나 24h 지났으면 재학습(dropped_at < now-1day 데이터만 → 무누수),
#          (2) 학습 컷오프 이후 ~ now-30min 사이 완료된 미채점 trip을 GBM+현재모델로 예측해 로깅.
# LightGBM(파이썬)이므로 Rust 실시간 경로 밖에서 배치로 돈다. cron 15분 주기.
import os, sys, json, pickle, warnings, datetime as dt
warnings.filterwarnings("ignore")
import numpy as np, pandas as pd, psycopg2
from lightgbm import LGBMRegressor

ROOT = "/home/tkadmin/projects/wp-tt-dashboard"
ART  = os.path.join(ROOT, "data")
MODEL_F = os.path.join(ART, "travel_gbm.pkl")     # {booster, baseline, glob, cutoff, trained_at}
CONN = dict(host="127.0.0.1", port=5433, user="wp", password=os.environ.get("PGPASSWORD", "wp"), dbname="wp_tt")
NUM = ["dist_m","hour","dow","congestion","density_50","density_100","density_150","density_200"]
CAT = ["origin","dest","origin_zone","dest_zone","shift"]
CLIP = (10, 3600)
RETRAIN_H = 24
SCORE_LAG_MIN = 30   # 완료 후 이만큼 지난 trip만 채점(밀도 등 피처 백필 대기)

def log(m): print(f"[{dt.datetime.now():%H:%M:%S}] {m}", flush=True)

def fetch(sql):
    with psycopg2.connect(**CONN) as c:
        return pd.read_sql(sql, c)

def prep(df):
    df = df.copy()
    for c in CAT:
        df[c] = df[c].astype("category")
    return df

def train():
    # 학습: 하루 이전 완료 trip만 (채점 대상과 겹치지 않게)
    df = fetch(
        "SELECT travel_s,dist_m,hour,dow,congestion,density_50,density_100,density_150,density_200,"
        "origin,dest,origin_zone,dest_zone,shift, dropped_at "
        "FROM learn_travel_sample WHERE travel_s IS NOT NULL AND dropped_at < now() - interval '1 day'")
    df = df[(df.travel_s >= CLIP[0]) & (df.travel_s <= CLIP[1])]
    if len(df) < 5000:
        log(f"train skip — only {len(df)} rows"); return None
    cutoff = df.dropped_at.max()
    glob = float(df.travel_s.median())
    baseline = df.groupby(["origin","dest"]).travel_s.median().to_dict()
    d = prep(df)
    gbm = LGBMRegressor(n_estimators=600, learning_rate=0.05, num_leaves=63,
                        min_child_samples=40, subsample=0.8, colsample_bytree=0.8, n_jobs=-1, verbose=-1)
    gbm.fit(d[NUM+CAT], np.log1p(d.travel_s), categorical_feature=CAT)
    m = dict(booster=gbm, baseline=baseline, glob=glob, cutoff=cutoff, trained_at=dt.datetime.now(dt.timezone.utc))
    os.makedirs(ART, exist_ok=True)
    with open(MODEL_F, "wb") as f: pickle.dump(m, f)
    log(f"trained on {len(df):,} rows | cutoff {cutoff:%Y-%m-%d %H:%M} | OD medians {len(baseline):,}")
    return m

def load_or_train():
    if os.path.exists(MODEL_F):
        with open(MODEL_F, "rb") as f: m = pickle.load(f)
        age_h = (dt.datetime.now(dt.timezone.utc) - m["trained_at"]).total_seconds()/3600
        if age_h < RETRAIN_H:
            return m
        log(f"model {age_h:.1f}h old — retraining")
    else:
        log("no model — training")
    return train() or (pickle.load(open(MODEL_F, "rb")) if os.path.exists(MODEL_F) else None)

def score(m):
    # 컷오프 이후 ~ now-lag 완료 · 미채점 trip
    df = fetch(
        "SELECT s.ytno,s.dropped_at,s.leg_ord,s.travel_s,s.dist_m,s.hour,s.dow,s.congestion,"
        "s.density_50,s.density_100,s.density_150,s.density_200,s.origin,s.dest,s.origin_zone,s.dest_zone,s.shift "
        "FROM learn_travel_sample s "
        f"LEFT JOIN travel_gbm_shadow g USING (ytno,dropped_at,leg_ord) "
        "WHERE g.ytno IS NULL AND s.travel_s IS NOT NULL "
        f"AND s.dropped_at > timestamptz '{m['cutoff']:%Y-%m-%d %H:%M:%S%z}' "
        f"AND s.dropped_at < now() - interval '{SCORE_LAG_MIN} minutes'")
    df = df[(df.travel_s >= CLIP[0]) & (df.travel_s <= CLIP[1])]
    if df.empty:
        log("no new trips to score"); return 0
    d = prep(df)
    gbm_s = np.clip(np.expm1(m["booster"].predict(d[NUM+CAT])), *CLIP)
    bl = m["baseline"]; gl = m["glob"]
    base_s = [bl.get((o, de), gl) for o, de in zip(df.origin, df.dest)]
    ta = m["trained_at"]
    rows = list(zip(df.ytno, df.dropped_at, df.leg_ord.astype(int), df.origin, df.dest,
                    df.dist_m.astype(float), df.travel_s.astype(int),
                    np.round(gbm_s).astype(int), np.round(base_s).astype(int), [ta]*len(df)))
    with psycopg2.connect(**CONN) as c, c.cursor() as cur:
        cur.executemany(
            "INSERT INTO travel_gbm_shadow (ytno,dropped_at,leg_ord,origin,dest,dist_m,actual_s,gbm_s,base_s,model_trained_at) "
            "VALUES (%s,%s,%s,%s,%s,%s,%s,%s,%s,%s) ON CONFLICT DO NOTHING",
            [(r[0],r[1],r[2],r[3],r[4],r[5],r[6],int(r[7]),int(r[8]),r[9]) for r in rows])
        c.commit()
    log(f"scored {len(df):,} trips")
    return len(df)

if __name__ == "__main__":
    m = load_or_train()
    if m is None:
        log("no model available — abort"); sys.exit(0)
    score(m)
