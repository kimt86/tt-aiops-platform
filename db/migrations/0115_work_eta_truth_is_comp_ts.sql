-- 0115: 작업도달(work-ETA) 예측의 정답지를 qc_move_log.st_ts → **comp_ts** 로 정정한다.
--       0113 을 같은 날 되돌리는 수정이다. 0113 은 틀린 컬럼을 정답지로 골랐다.
--
-- ■ 0113 이 무엇을 잘못했나
-- 0113 은 "적하도 크레인 기록이 있다"는 발견(맞음)까지는 옳았으나, 그 기록에서 **`st_ts` 를
-- '크레인이 이 컨테이너를 실제로 집기 시작한 시각'으로 읽었다**. 아니었다.
--
--   **MCH_OPERATION(=안벽 QC 무브 로그)의** `st_ts` (= ST_DT) 는 물리 시작이 아니라, 완료 시점에
--   소급 기록되는 **큐잉/배차 시각**이다.
--   `comp_ts` (= MCH_OPER_COMPDATE||MCH_OPER_COMPTIME) 가 **크레인↔트럭 물리 핸드오버 완료**다.
--
-- ■ 근거 — 서로 독립적인 네 갈래 (2026-08-03 실측, 별도 표기 없으면 최근 24시간)
--   ① **크레인 동시성(가장 결정적·아무 표와도 조인하지 않음)**: 크레인은 한 번에 한 개만 든다.
--      st_ts 가 물리 시작이라면 같은 크레인의 [st_ts, comp_ts] 구간이 겹칠 수 없다. 스윕라인 실측
--      (최근 12h, QC 59대 전부): 크레인당 **동시 열림 중앙 7개**(평균 7.63·최대 28), **59대 전부가
--      최대 5개 이상**, 한 번도 안 겹치는 크레인 **0대**. 트윈 리프트로는 2까지밖에 설명이 안 된다.
--   ② **comp_ts 는 반대로 완벽히 직렬**: 크레인별 연속 comp_ts 간격 p50 83~112초(시간당 32~43무브
--      = 실제 QC 생산성), 같은 초 겹침 **0.0~0.3%**. 반면 st_ts 는 같은 초 공유가 17.7~33.5%
--      (한 초에 최대 4행) — 배치 발행의 서명이지 물리 이벤트가 아니다.
--   ③ **GPS 교차검증(TOS 표와 무관)**: 핸드오버라면 그 순간 트럭이 크레인 밑에 있어야 한다.
--      comp_ts 시점 트럭 위치는 크레인 중심에서 중앙 **23~47m**(= QC 한 대 발자국), st_ts 시점은
--      중앙 **349~758m**(p90 990~2,414m·터미널 전역 산개).
--   ④ **다른 Oracle 표와의 항등**: tos_handover_label.dis_ts(= JOB_ORDER_HISTORY 의 YT_DIS_DT)와
--      직접 대조 시 st_ts = dis_ts 가 **양하 98.92% / 적하 99.57%**(초 단위 완전일치).
--
--   ⚠ **인용하면 안 되는 근거**: "comp_ts = tt_move_log.pickup_ts(DS)/free_ts(LD) 가 100%" 는
--      **동어반복**이다. tt_move_log 자체가 mig0092 에서 qc_move_log.comp_ts 로 조립되기 때문이다.
--      (이 마이그레이션 초판이 이걸 근거로 들었다가 적대 검증에서 지적받아 걷어냈다.)
--
-- ■ ⚠⚠ 범위 한정 — RTG 에 같은 수정을 하면 그게 새 오류다
--   확립된 명제는 "**QC(MCH_OPERATION) 의** ST_DT 는 물리 시작이 아니다"이지 "ST_DT 라는 컬럼이
--   언제나 배차시각이다"가 아니다. `rtg_move_log` 에서는 같은 이름의 컬럼이 배차와 **0.03%만**
--   일치하고(중앙 오프셋 DS +1,239초) 물리 시작처럼 행동한다(comp−st 51~118초·구간 겹침 2.3%).
--   야드 쪽 정답지를 손볼 때 이 문서를 근거로 삼지 말 것.
--
-- ■ ⚠ 저장소는 이미 이걸 적어 두고 있었다 (내가 안 읽었다)
--   kc/data/tos-db-reference.html:58 — "ST_DT | move 시작(단, **완료 시 소급 기록되는 큐잉 시각**)"
--   kc/data/cycle-decomposition.html:90 — 같은 서술 + "간격 극가변", QC/RTG 비대칭까지 관찰돼 있음
--   kc/dispatch/leadtime-adr.html — 2026-07-15 Oracle 직접조회로 **행이 완료 시점에 생성되고 ST_DT
--     는 소급 기입**됨을 실측(진행중 0/3,301건·행생성−ST_DT 276초)해 실시간 리드타임 시도를 기각
--   crates/extractor/src/qc_moves.rs 헤더 · db/migrations/0087 — **comp_ts 만** "물리 핸드오버 완료"
--   ⇒ 0113 은 컬럼 이름(st = start)으로 의미를 추정하고, 그 컬럼을 이미 정의해 둔 우리 문서를
--     확인하지 않았다. **새 정답지를 채택하기 전에 KC 의 해당 표 문서를 먼저 읽을 것.**
--
-- ■ 왜 0113 의 검증이 이 오류를 잡지 못했나 (이게 진짜 교훈)
-- 0113 은 "정답지를 통일하니 양하·적하 잔차가 −138초로 **똑같아졌다**"를 성공 신호로 삼았다.
-- 그 대칭은 사실 **오류의 증상**이었다. st_ts=배차시각이고 우리 예측도 배차 국면에 앵커되어 있어,
-- 둘의 차이가 작업 유형과 무관하게 비슷해진 것뿐이다. 진짜 정답지(comp_ts)로 재면 잔차는
-- 양하 +264초 / 적하 +1,004초로 **비대칭이 실재한다**.
--   ⇒ 교훈: "두 갈래가 같아졌다"는 정답지가 옳다는 증거가 아니다. 새 정답지는 **그 컬럼이 무엇인지
--     독립적으로(물리 제약·소스 스키마·다른 표와의 항등) 확인**하고 나서 채택할 것.
--     0113 은 컬럼 이름(st=start)만 보고 의미를 추정했다.
--
-- ⚠ 남는 한계: comp_ts 는 핸드오버 **완료**이지 크레인이 그 컨테이너에 손을 대기 시작한 시점이 아니다.
--   work_eta_ts 를 '크레인 착수'로 해석한다면 comp_ts 는 한 사이클(60~180초)만큼 늦다. 다만 comp_ts
--   는 물리 이벤트이고 st_ts 는 아니므로 정답지로는 comp_ts 가 압도적으로 낫다. 그리고 배차 관점에서
--   트럭이 있어야 하는 시각은 착수가 아니라 **핸드오버 자체**라 comp_ts 가 의미적으로도 맞다.
--
-- ■ 부수 소득 — 이 실측이 TOS 의 선행시간을 준다
--   comp_ts − st_ts = TOS 가 배차하고 나서 크레인 핸드오버까지 걸린 시간
--     중앙 양하 **473초(7.9분)** · 적하 **1,672초(27.9분)**
--   우리 Stage-2 그림자는 "지금 노는 트럭으로 지금 필요한 작업을 즉시 채운다"라 선행시간이 사실상 0이다.
--   실행가능률이 현장 기아율(6~16%)과 4~5배 어긋나던 이유가 여기 있다(별건으로 처리).
--
-- ■ 이 마이그레이션이 하는 일
-- 1) 새 정답지 태그 `resolved_src = 'qc_comp'` 를 도입한다. 옛 `'qc'` (= st_ts 기반, 0113 이 쓴 값)는
--    **건드리지 않고 그대로 둔다** — 매뷰가 'qc_comp' 만 보므로 오염된 옛 값은 자동으로 빠진다.
--    행을 지우거나 NULL 로 되돌리지 않는 이유: 라이브 표에 대한 대량 UPDATE 를 피하고(운영 DB),
--    옛 값이 남아 있어야 나중에 두 정답지를 나란히 비교할 수 있다.
-- 2) 매뷰를 'qc_comp' 기준으로 재생성. 나머지 규칙(원본예측 대비 잔차·창을 원본예측 기준으로 절단·
--    최소표본)은 0113 그대로 옳으므로 유지한다.
-- 3) 인덱스는 **추가하지 않는다**. 기존 qc_move_log_cont_idx (contno, jobtype, st_ts) 의 선두 두
--    컬럼으로 충분하다(contno 당 행수 중앙 1·최대 2). 실측 계획: 9.5ms.
--
-- ⚠ 전환기: 매뷰가 다시 빈다. 새 규칙 표본이 쌓일 때까지 보정 0 = 원본예측을 그대로 쓴다.
--    적하 기준 마감이 지금보다 **약 17분 이르게** 잡히므로(보수적 = 더 급하게) 안전한 쪽이다.
--
-- 멱등.

-- 컬럼 자체에 못 박는다. `\d qc_move_log` 로 의미를 확인하려는 사람이 제일 먼저 보는 자리이고,
-- 0087 스키마 주석의 `st_ts -- move start (ST_DT)` 한 줄이 0113 오류의 씨앗이었다.
COMMENT ON COLUMN qc_move_log.st_ts IS
  '⚠ TOS 배차 시각(ST_DT = JOB_ORDER_HISTORY.YT_DIS_DT 와 초 단위 일치·DS 98.9%/LD 99.6%). '
  '크레인의 물리적 작업 시작이 **아니다** — 무브 완료 시점에 소급 기입되며, 같은 크레인의 '
  '[st_ts, comp_ts] 구간이 90.6% 겹친다(한 번에 하나만 드는 크레인에서 불가능). '
  '"크레인이 이 컨테이너를 언제 다뤘나"의 정답지로 쓰지 말 것 — comp_ts 를 쓰라(mig 0115). '
  'RTG(rtg_move_log)의 동명 컬럼은 반대로 진짜 물리 시작이니 혼동 금지.';
COMMENT ON COLUMN qc_move_log.comp_ts IS
  '크레인↔트럭 물리 핸드오버 완료(MCH_OPER_COMPDATE||COMPTIME). 이 표에서 유일한 물리 이벤트이고 '
  '작업도달 예측의 권위 정답지다. 같은 크레인 연속 comp_ts 간격 p50 83~112초 = 실제 QC 생산성.';
COMMENT ON COLUMN qc_move_log.dur_s IS
  '⚠ comp_ts − st_ts = **배차 → 핸드오버 선행시간**(중앙 DS ~466초 / LD ~1,654초). '
  '크레인 작업시간이 아니다(실제 리프트 사이클은 ~85초). Stage-2 실행가능 판정이 이 값을 쓴다'
  '(learn_dispatch_lead·mig 0116). 추출기 상한이 3600초이던 시절 적하 9.3%가 NULL 로 잘렸으니 '
  '과거 구간을 볼 때는 dur_s 대신 comp_ts − st_ts 를 직접 빼라.';

COMMENT ON COLUMN dispatch_pred_sample.resolved_src IS
  'qc_comp = qc_move_log.comp_ts (크레인↔트럭 물리 핸드오버 완료·권위 정답지) · '
  'pool = 작업풀 이탈 대체값(늦음) · '
  'qc = [폐기·0113] qc_move_log.st_ts 였는데 그건 크레인 시작이 아니라 배차시각이었다(0115 참조). '
  '보정 학습은 qc_comp 만 쓴다.';

DROP MATERIALIZED VIEW IF EXISTS learn_work_eta_bias;
CREATE MATERIALIZED VIEW learn_work_eta_bias AS
  SELECT COALESCE(qc, '')::text AS qc,
         jobtype,
         count(*)::integer      AS n,
         percentile_cont(0.5) WITHIN GROUP (
           ORDER BY EXTRACT(epoch FROM (resolved_at - (pred_work_eta_ts - make_interval(secs => applied_bias_s))))::float8
         )::integer             AS med_err_s
    FROM dispatch_pred_sample
   WHERE resolved_at IS NOT NULL
     AND jobtype IS NOT NULL
     AND resolved_src = 'qc_comp'       -- 크레인 물리 핸드오버만. 'qc'(옛 st_ts)·'pool' 제외.
     AND applied_bias_s IS NOT NULL     -- 원본예측을 복원할 수 있는 행만
     AND logged_at > now() - interval '7 days'
     -- 표본 창은 **원본예측** 기준. 보정된 값으로 자르면 보정이 커질수록 모집단이 이동한다.
     AND (pred_work_eta_ts - make_interval(secs => applied_bias_s) - logged_at)
           BETWEEN interval '5 min' AND interval '45 min'
   GROUP BY GROUPING SETS ((qc, jobtype), (jobtype))
  HAVING count(*) >= 50;

CREATE UNIQUE INDEX IF NOT EXISTS learn_work_eta_bias_pk ON learn_work_eta_bias (qc, jobtype);
