-- 0106: 야드 좌표의 물리 상한을 스키마에 못박는다.
--
-- 왜 코드 가드로 부족한가 (2026-07-28 전수조사 후속):
--   scenario.yard_cell 은 `generate_series(1, tier)` 로 채워진다. tier 는 Oracle 의
--   CRNT_PSN_IDX_NO4 + 1 을 검증 없이 그대로 쓴 값이라, 손상된 한 행이 쓰기 "행 수"를
--   입력 값으로 정해버린다 — 같은 날 서버를 내린 래스터 폭주와 같은 유형이다.
--   차이는 방어 수단이다: 래스터는 프로세스 메모리라 MemoryMax 로 막히지만, 이쪽은
--   실제 삽입 실행자가 Postgres(다른 cgroup)라 프로세스 상한이 전혀 듣지 않고,
--   느린 게 아니라 그냥 큰 것이라 statement_timeout 으로도 안 잡힌다.
--
--   커밋 acb8677 이 MAX_TIER=10 을 Rust 두 곳(수집 관문·할당 지점)에 넣었다. 그건
--   오늘의 코드 경로를 막는다. 막지 못하는 것: 백필 스크립트, 수동 SQL, 앞으로 생길
--   다른 쓰기 경로. 물리 법칙은 코드가 아니라 스키마가 알고 있어야 한다.
--
-- 경계값(실측 2026-07-28, yard_cell 92,765행 · yard_move 247,566행):
--   tier    1..7   → 상한 10 (Rust MAX_TIER 와 일치)
--   bay_idx 0..41  → 0..500
--   row_idx 0..17  → 0..100
--   ⚠ tier 는 1-based 인데 bay_idx/row_idx 는 0-based 다. 처음에 셋 다 1부터라고
--     가정했다가 이 제약이 실제 행 위반으로 즉시 잡아냈다(bay<1 이 cell 4,376행·
--     move 11,887행, row<1 이 15,964·41,383행). 잘못된 상한을 조용히 넣지 않은 게
--     제약을 스키마에 두는 이유 그 자체다.
--   tier 는 코드 게이트가 먼저 걸러서 평시에 발화하지 않는다 — 발화하면 게이트를
--   우회한 경로가 있다는 뜻이고, 그때는 조용히 10만 행을 쓰는 것보다 시끄럽게
--   실패하는 쪽이 옳다. bay/row 는 행 수를 정하진 않지만 같은 미검증 Oracle 출처이고
--   PK 구성요소라 손상값이 쓰레기 셀을 만든다. 실측의 10배 이상으로 넉넉히 잡는다.
--
-- 멱등: db/apply.sh 가 전체 파일을 매번 실행하므로 존재 검사 후 추가한다.

DO $$
DECLARE
  c record;
BEGIN
  FOR c IN
    SELECT * FROM (VALUES
      ('yard_cell', 'yard_cell_tier_physical', 'tier BETWEEN 1 AND 10'),
      ('yard_cell', 'yard_cell_bay_physical',  'bay_idx BETWEEN 0 AND 500'),
      ('yard_cell', 'yard_cell_row_physical',  'row_idx BETWEEN 0 AND 100'),
      ('yard_move', 'yard_move_tier_physical', 'tier IS NULL OR tier BETWEEN 1 AND 10'),
      ('yard_move', 'yard_move_bay_physical',  'bay_idx IS NULL OR bay_idx BETWEEN 0 AND 500'),
      ('yard_move', 'yard_move_row_physical',  'row_idx IS NULL OR row_idx BETWEEN 0 AND 100')
    ) AS t(tbl, cname, expr)
  LOOP
    IF to_regclass('scenario.' || c.tbl) IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = c.cname) THEN
      EXECUTE format('ALTER TABLE scenario.%I ADD CONSTRAINT %I CHECK (%s)', c.tbl, c.cname, c.expr);
    END IF;
  END LOOP;
END $$;
