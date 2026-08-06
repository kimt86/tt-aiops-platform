-- 0133: 레거시 풀(TOS 미배차 한정 + 크레인당 지평 캡) 제거에 따른 컬럼 정리.
-- 레거시는 방법부터 틀렸다: ①미배차만 담아 마감 임박 작업이 구조적으로 존재할 수 없었고
-- (남은 시간 최소가 829초에서 잘림) ②지평 캡 900초 고정이라 적하 요구 선행 p90 1,693초가
-- 들어갈 수 없어 적하 실행가능률이 0.0%였다. 되돌리지 않으므로 킬스위치째 걷어낸다.
-- 컬럼은 지우지 않는다 — 21일 보존 시계열의 과거 구간이 살아 있다. NULL 로 멈추고 경계를 적는다.
ALTER TABLE stage2_solver_shadow ADD COLUMN IF NOT EXISTS due_buckets_n integer;
COMMENT ON COLUMN stage2_solver_shadow.due_buckets_n IS
  '이 틱에 배차 마감이 도래한 묶음 수(트럭 수로 자르기 **전**). 옛 works_raw 를 대신한다 — '
  '수요가 적은 건지 트럭이 모자라 잘린 건지 구분하는 값 (mig 0133).';
DO $$
DECLARE c text;
BEGIN
  FOREACH c IN ARRAY ARRAY['dep_tier_on','dep_urgent_slots','dep_demoted_n','ab_block',
                           'ab_warmup','need_horizon_on','works_raw','pool_overlap_n']
  LOOP
    EXECUTE format(
      'COMMENT ON COLUMN stage2_solver_shadow.%I IS %L', c,
      '⚠2026-08-06 폐기 — 레거시 풀 제거로 원천이 사라졌다. 그 이후 행은 항상 NULL. '
      '과거 구간(pool_mode IS NULL)에서만 의미가 있다 (mig 0133).');
  END LOOP;
END $$;
COMMENT ON COLUMN stage2_pool_shadow.in_current_pool IS
  '⚠2026-08-06 폐기 — 레거시 풀이 사라져 비교 대상이 없다. 이후 행은 NULL (mig 0133).';
COMMENT ON COLUMN stage2_pool_shadow.rank_current IS
  '⚠2026-08-06 폐기 — 위와 같음. 이후 행은 NULL (mig 0133).';
COMMENT ON COLUMN stage2_solver_shadow.pool_mode IS
  '매칭을 구동한 풀. NULL=전환 전(레거시), 1=설계③ 마감 풀. 2026-08-06 레거시 제거 후로는 '
  '항상 1 이다(0=레거시 킬스위치는 존재하지 않는다). 과거 구간과 가르는 데 계속 쓴다 (mig 0132·0133).';
