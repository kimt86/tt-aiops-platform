# HANDOFF — 다음 사이클

마지막 갱신 2026-08-12. 앞선 사이클: 배차 탐지 지연(머지 `412901c`, main 에 푸시됨).

> 상세 기준선·함정은 `~/.claude/notes/tt-aiops-platform.md` 2026-08-11~12 절에 있다.
> 이 파일은 **다음에 무엇을 할지**만 담는다.

---

## DONE CRITERIA (작업을 끝냈다고 말하기 전에 전부 돌릴 것)

```bash
cargo build --release -p tt-api && cargo build --release -p tt-extractor
cargo test --workspace                      # 65 통과 · 실패 0 이 기준
systemctl --user is-active tt-api           # active
```
```sql
-- 배차 파이프라인이 살아 있는가 (틱 60/시간 · 경보 0 · 작업목록 6~15초)
SELECT EXTRACT(epoch FROM now()-max(ts))::int AS 마지막틱_초전,
       (SELECT count(*) FROM stage2_solver_shadow WHERE ts > now()-interval '1 hour') AS 최근1h_틱,
       (SELECT count(*) FROM ops_alert WHERE last_ts > now()-interval '3 hours') AS 최근3h_경보,
       (SELECT round(avg(workpool_age_s),1) FROM stage2_solver_shadow
         WHERE ts > now()-interval '1 hour') AS 목록나이_평균초
  FROM stage2_solver_shadow;

-- ★새 게이지: 매칭이 낡은 목록을 쓴 틱 (0 이 아니면 위상 문제)
SELECT count(*) FILTER (WHERE workpool_age_s > 45) AS 위상풀림_틱,
       count(*) AS 전체틱
  FROM stage2_solver_shadow WHERE ts > now()-interval '1 day';
```
```bash
# 유닛 드리프트 0 (저장소 == 설치본)
for f in deploy/systemd/*.service deploy/systemd/*.timer; do
  b=$(basename "$f"); i="$HOME/.config/systemd/user/$b"
  [ -f "$i" ] || { echo "미설치: $b"; continue; }
  diff -q "$f" "$i" >/dev/null || echo "차이: $b"
done
```
2026-08-12 실측: 65/0 · active · 틱 60/시간 · 경보 0 · 목록나이 평균 15.1초 · 드리프트 0.

---

## 지금 참인 것 (전에는 아니었던 것)

- **매칭 틱이 매분 :15 에 고정**됐다. 종전에는 위상이 `tt-api` 프로세스가 시작한 초라
  재배포마다 0~59초에서 재추첨됐다. 재시작 8회로 확인.
  ⚠ 이 값은 `tt-workpool.timer` 의 `*:*:55` 와 **한 쌍**이다.
- **TOS 배차 시각의 권위값은 `live_workpool.yt_dis_ts`**(= `JOB_ORDER_LIST.YT_DIS_DT`).
  `upd_ts`(UPD_DT)는 행 마지막 갱신이라 배차 이후 갱신에 밀린다(격차 p90 1,382초).
  ⚠ `YT_DIS_DT` 는 **VARCHAR2(14)** — `TO_CHAR` 금지.
- **비교기가 올바른 순간으로 되감는다**(T1 = `yt_dis_ts`). 종전에는 배차행 51.4%가 실제 배차
  보다 뒤(p90 2,170초)의 순간을 보고 있었다. 판별자 `t1_ver`·실제 값 `t1_ts`, 소비처 4곳이
  `t1_ver = 1` 로 거른다.
- **매칭이 실제로 쓴 목록 나이가 틱마다 기록된다**(`stage2_solver_shadow.workpool_age_s`).
- `mig0048` 의 "`YT_DIS_DT`=after-arrival" 서술이 **틀렸음이 확정**됐다(`0092`·`0115` 가 맞다).

## 일부러 안 한 것

- **`MATCH_TICK_SEC` 을 :15 에서 옮기지 않았다** — 아래 후보 1번 참조. 하루치로 또 정하면
  같은 실수를 반복하는 것이라, 며칠 쌓고 정하기로 했다(사용자 A안 승인 2026-08-12).
- **경계 이전 비교 기록 77만 행을 백필하지 않았다.** 자료는 있으나(`truck_pos_hist` 2일치)
  비용이 크고, `NOT EXISTS` 가 `tos_upd` 로 걸려 경계 시점에 남아 있던 행은 재비교가 영구히
  막힌다. `t1_ver` 로 갈라 읽으면 된다.
- **`workpool.rs:841` block(0) 의 `tos_upd_dt` 는 UPD_DT 그대로 뒀다** — 이건 "상한값"이라고
  이름 붙여 기록하는 자리라 잘못된 계산이 아니라 정직한 라벨이다. 권위값은 `tos_dis_ts` 에.
- **`fair_compare` 두 표에 행 단위 판별자를 안 달았다.** 틱 집계라 소급이 불가능해 COMMENT 로
  경계만 적었다. 21일 보관이라 3주간은 `ts` 로 직접 갈라야 한다.
- **게이지에 경보를 안 달았다.** 임계는 정상 대역이 며칠 쌓인 뒤 정하는 편이 맞다.

## 이번 사이클에서 나온 범위 밖 발견

- **★탐지 지연의 45%는 폴링으로 못 줄인다.** 새로 보이는 배차행의 45%가 "배차는 중앙 24분 전
  (최대 57분)인데 이제야 목록에 들어온" 행이다. `upd_ts` 로 보면 "36초 전"이라 **대리값으로는
  이 모집단이 안 보였다.** 원인 미규명(재발행 / 상태 전이로 모집단 늦게 진입 / 키 변경).
  깜빡임은 배제(0/95). ⇒ **A2 판단의 전제가 바뀐다**(아래 후보 2번).
- `dispatch_compare_shadow` 는 TOS 가 UPD_DT 를 밀 때마다 **같은 배차가 새 행으로 들어온다**
  (실측 47%가 2행 이상·최대 6행). 집계 전 `DISTINCT ON (qc,queuename,tos_ytno,t1_ts)` 필요.
- 기존 경보 2건이 미해소로 남아 있다: `disk/filesystem`(마지막 08-10 00:38, 이후 재발 없음 ·
  현재 여유 94GiB · 래치 행), `deadman/road_route_eval`(08-03).
- `web/public/livemap-roadgraph.geojson` 은 매시 크론이 다시 써서 항상 modified 로 뜬다.

## 다음 후보 (한 줄 근거)

1. **★매칭 위상 재검토 — `MATCH_TICK_SEC` 또는 설계 변경(C안).**
   배포 첫날 게이지가 **927틱 중 10건(1.1%)에서 52~71초 된 목록**을 잡았다(08-12 07:53~08:04,
   착지가 +20~30초로 늦어진 구간). :15 의 근거였던 "관측 최대 착지 +14초"(6시간 표본)가 하루
   만에 깨졌다. 선택지 둘:
   - **값만 옮기기**: :35 쯤이면 오늘 관측(+30초)은 덮지만 목록이 그만큼 묵는다. 또 짧은
     표본으로 정하는 문제가 남는다.
   - **★C안 — 고정 초 대신 "새 목록이 오면 돈다"**(사용자 지시로 기록, 2026-08-12).
     `data_freshness(WORKPOOL).last_success_at` 이 바뀌는 것을 신호로 매칭을 깨우면 상수 자체가
     사라지고 타이머와의 짝 맞추기 부담도 없어진다. 설계 변경이라 별도 사이클.
   ⚠ `crates/api/src/livemap.rs` 의 테스트 `목표_초는_관측된_최대_착지보다_뒤에_있다` 는
   상수 `관측_최대_착지_초 = 9` 를 쓴다. **실측값(:25)으로 갱신하면 이 테스트는 실패한다** —
   그게 이 항목이 1순위인 이유다.
2. **A2 (workpool 폴링 60초 → 30초) — 전제 재검토 후 판단.**
   위 "범위 밖 발견" 때문에 이득이 종전 추정보다 작다(신규 배차의 55%에만 듣는다). 게다가
   1분 유닛 4개가 20초 격자를 채워 **30초 간격 슬롯 쌍이 없고**, `ORACLE_LOCK` 이 프로세스
   안에서만 돌아 유닛 간 직렬화가 없다. 격자 재설계 + 부하 결정이 필요하다.
3. **TOS 기술 세션** — `docs/tos-integration-handoff.md` 의 7개 질문. 기술 쪽 유일한 크리티컬
   패스. ★탐지 지연도 여기 걸려 있다: TOS 가 우리 추천을 소비하면 "TOS 배차를 탐지"할 필요가
   줄고(우리가 보낸 것의 ack 를 받으면 되므로), Oracle push(CDC·트리거·AQ)는 우리가 못 만든다
   — 읽기 전용 폴링이 유일한 경로다(디스커버리 확인: CDC/Kafka/Debezium 0건).
4. **운영자 채택/기각 기록 장치** — 파일럿 Phase 1→2 통과 기준이 "운영자 수용"인데 재는 장치가
   없다. 지금의 46.9%("TOS 와 같은 상자")는 사람의 판단이 아니다.
5. **비교기 지표 재측정** — T1 절체 후 표본이 전부 한산한 시간대(풀 1~2건)에서 나왔다.
   평시 표본이 쌓인 뒤 `t1_ver = 1` 로 갈라 다시 볼 것. 지금 숫자로 "개선됐다"고 말하면 안 된다.
6. **평문 비밀번호 정리** — `scripts/*.sh` 의 `PGPASSWORD=wp`. GitHub 원격이 있다.
7. **`scripts/unit_failed_alert.sh` 를 scengen 유닛에도** — 담당이 달라 합의 필요.
8. **디스크 root 영역** — 08-09 에 여유 20GiB 밑까지 갔다. 현재 94GiB 로 회복. root 권한자 몫.

## 사용자가 답해야 하는 것

- **위상 문제를 값 조정으로 갈 것인가, C안(목록 도착을 신호로)으로 갈 것인가?**
  자료가 며칠 쌓이면 값 조정의 근거가 단단해지지만, C안은 상수 자체를 없앤다.
- **`DISPATCH_MODE=active` 를 언제 켤 것인가?** 켜는 순간의 유일한 효과는 "직전 180초에 우리가
  추천한 상자를 풀에서 뺀다"이고, TOS 소비 채널이 없는 지금은 얻는 것이 없다(실측: 후보 묶음의
  80%가 자기 추천 적중 — 켜면 첫 틱에 그만큼 빠진다).
- **TOS 에 "냉동·위험물·OOG 배차에 사람이 지키는 절차가 있는가"를 물을 것인가?** 있다면
  "빼지 않는다" 결정을 뒤집어야 한다. 우리 자료로는 확인 불가.
